'use strict';

const hookedServices = new Set();
const hookedLocators = new Set();

function hex(bytes) {
  return Array.from(new Uint8Array(bytes))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

function ts() {
  return new Date().toISOString();
}

function isWbemLocator(iid) {
  try {
    // IID_IWbemLocator = dc12a687-737f-11cf-884d-00aa004b2e24
    return hex(iid.readByteArray(16)) === '87a612dc7f73cf11884d00aa004b2e24';
  } catch (_) {
    return false;
  }
}

function readBstr(value) {
  try {
    return value.isNull() ? '<NULL>' : value.readUtf16String();
  } catch (_) {
    return '<unreadable>';
  }
}

function dumpInputObject(input) {
  if (input.isNull()) {
    console.log('  input: NULL');
    return;
  }

  try {
    const vtable = input.readPointer();
    const beginEnumeration = new NativeFunction(
      vtable.add(8 * Process.pointerSize).readPointer(),
      'int',
      ['pointer', 'int'],
    );
    const next = new NativeFunction(
      vtable.add(9 * Process.pointerSize).readPointer(),
      'int',
      ['pointer', 'int', 'pointer', 'pointer', 'pointer', 'pointer'],
    );
    const endEnumeration = new NativeFunction(
      vtable.add(10 * Process.pointerSize).readPointer(),
      'int',
      ['pointer'],
    );
    const variantClear = new NativeFunction(
      Module.getGlobalExportByName('VariantClear'),
      'int',
      ['pointer'],
    );
    const sysFreeString = new NativeFunction(
      Module.getGlobalExportByName('SysFreeString'),
      'void',
      ['pointer'],
    );

    const hr = beginEnumeration(input, 0);
    if (hr !== 0) {
      console.log('  BeginEnumeration HRESULT=0x' + (hr >>> 0).toString(16));
      return;
    }

    for (let i = 0; i < 32; i++) {
      const nameOut = Memory.alloc(Process.pointerSize);
      nameOut.writePointer(NULL);
      const variant = Memory.alloc(16);
      for (let j = 0; j < 16; j++) variant.add(j).writeU8(0);
      const cimType = Memory.alloc(4);
      const flavor = Memory.alloc(4);

      const nextHr = next(input, 0, nameOut, variant, cimType, flavor);
      if (nextHr !== 0) break;

      const namePtr = nameOut.readPointer();
      const name = namePtr.isNull() ? '<unnamed>' : namePtr.readUtf16String();
      const vt = variant.readU16();
      let value = '<VT 0x' + vt.toString(16) + '>';

      if (vt === 21) { // VT_UI8
        value = '0x' + variant.add(8).readU64().toString(16);
      } else if (vt === 17) { // VT_UI1
        value = '0x' + variant.add(8).readU8().toString(16).padStart(2, '0');
      } else if (vt === 19) { // VT_UI4
        value = '0x' + variant.add(8).readU32().toString(16);
      } else if (vt === 3) { // VT_I4
        value = String(variant.add(8).readS32());
      } else if (vt === 8) { // VT_BSTR
        const bstr = variant.add(8).readPointer();
        if (bstr.isNull()) {
          value = '<NULL BSTR>';
        } else {
          const text = bstr.readUtf16String();
          value = '"' + text + '"';
          if (/^\d+$/.test(text)) {
            try {
              value += ' (u64=0x' + BigInt(text).toString(16).padStart(16, '0') + ')';
            } catch (_) {
              // Keep the original BSTR when BigInt is unavailable.
            }
          }
        }
      } else if (vt === (0x2000 | 17)) { // VT_ARRAY | VT_UI1
        const safeArray = variant.add(8).readPointer();
        if (!safeArray.isNull()) {
          const count = safeArray.add(24).readU32();
          const data = safeArray.add(16).readPointer();
          const length = Math.min(count, 256);
          value = 'bytes[' + count + '] ' + hex(data.readByteArray(length));
        }
      }

      console.log('  input.' + name + ' vt=0x' + vt.toString(16) + ' value=' + value);
      if (!namePtr.isNull()) sysFreeString(namePtr);
      variantClear(variant);
    }

    endEnumeration(input);
  } catch (e) {
    console.log('  input decode failed: ' + e);
  }
}

function hookServices(services) {
  if (services.isNull()) return;

  const key = services.toString();
  if (hookedServices.has(key)) return;
  hookedServices.add(key);

  const vtable = services.readPointer();
  // IWbemServices::ExecMethod is vtable slot 24 (IUnknown slots included).
  const execMethod = vtable.add(24 * Process.pointerSize).readPointer();

  console.log('[+] IWbemServices hooked: ' + services + ' @ ' + execMethod);

  Interceptor.attach(execMethod, {
    onEnter(args) {
      const objectPath = readBstr(args[1]);
      const methodName = readBstr(args[2]);
      const input = args[5];

      if (methodName.indexOf('Gaming') !== -1 || objectPath.indexOf('AcerGamingFunction') !== -1) {
        console.log('\n[' + ts() + '][ExecMethod]');
        console.log('  object: ' + objectPath);
        console.log('  method: ' + methodName);
        console.log('  pInParams: ' + input);
        dumpInputObject(input);
        console.log('  thread: ' + Process.getCurrentThreadId());
      }
    },
  });
}

function hookLocator(locator) {
  if (locator.isNull()) return;

  const key = locator.toString();
  if (hookedLocators.has(key)) return;
  hookedLocators.add(key);

  const vtable = locator.readPointer();
  // IWbemLocator::ConnectServer is vtable slot 3 (IUnknown slots included).
  const connectServer = vtable.add(3 * Process.pointerSize).readPointer();

  console.log('[+] IWbemLocator hooked: ' + locator + ' @ ' + connectServer);

  Interceptor.attach(connectServer, {
    onEnter(args) {
      // ConnectServer(..., pCtx, ppNamespace, ppCallResult): ppNamespace is arg 8.
      this.ppNamespace = args[8];
      console.log('[ConnectServer] called');
    },
    onLeave(retval) {
      if (retval.toInt32() === 0 && this.ppNamespace && !this.ppNamespace.isNull()) {
        try {
          hookServices(this.ppNamespace.readPointer());
        } catch (e) {
          console.log('[!] Could not read IWbemServices: ' + e);
        }
      }
    },
  });
}

const ole32 = Process.getModuleByName('ole32.dll');
const coCreateInstance = ole32.findExportByName('CoCreateInstance');

if (coCreateInstance === null) {
  throw new Error('CoCreateInstance was not found in ole32.dll');
}

console.log('[*] Frida WMI trace loaded');
console.log('[*] Hooking CoCreateInstance and CoCreateInstanceEx');

Interceptor.attach(coCreateInstance, {
  onEnter(args) {
    // CoCreateInstance(rclsid, pUnkOuter, clsctx, riid, ppv)
    this.iid = args[3];
    this.ppv = args[4];
  },
  onLeave(retval) {
    if (retval.toInt32() !== 0) return;

    try {
      const iid = hex(this.iid.readByteArray(16));
      const iface = this.ppv.readPointer();
      console.log('[CoCreateInstance] iid=' + iid + ' iface=' + iface);
      if (isWbemLocator(this.iid)) hookLocator(iface);
    } catch (e) {
      console.log('[!] Could not inspect CoCreateInstance: ' + e);
    }
  },
});

const coCreateInstanceEx = ole32.findExportByName('CoCreateInstanceEx');
if (coCreateInstanceEx !== null) {
  Interceptor.attach(coCreateInstanceEx, {
    onEnter(args) {
      this.pResults = args[5];
    },
    onLeave(retval) {
      if (retval.toInt32() !== 0 || this.pResults.isNull()) return;

      try {
        // MULTI_QI: pIID, pItf, HRESULT.
        const iid = this.pResults.readPointer();
        const iface = this.pResults.add(Process.pointerSize).readPointer();
        console.log('[CoCreateInstanceEx] iid=' + hex(iid.readByteArray(16)) + ' iface=' + iface);
        if (isWbemLocator(iid)) hookLocator(iface);
      } catch (e) {
        console.log('[!] Could not inspect CoCreateInstanceEx: ' + e);
      }
    },
  });
}
