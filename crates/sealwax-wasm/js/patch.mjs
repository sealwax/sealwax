// The glue wasm-bindgen writes, made safe to start again after a panic.
//
// A panic aborts the module mid-call (a trap): its memory is left as it was, and every call after
// it would read that state. The glue keeps one instance per page, so nothing could be done but
// reload the page. Here:
// - the module's functions are guarded: after a trap, each call throws `SealwaxCrashedError`;
// - `__sealwax_forget` drops the instance, so that the next initialization makes another;
// - every object is stamped with the instance it belongs to: one from before a reset is refused
//   (its pointer means nothing in the new memory), and the garbage collector frees it nowhere.
//
// node patch.mjs GLUE ERRORS: GLUE is the generated `sealwax.js`, ERRORS the path its `makeError`
// is imported from. wasm-bindgen is pinned (build.sh checks it): each change below must apply,
// or the build fails rather than ship an unguarded module.

import { readFileSync, writeFileSync } from "node:fs";

const [glue, errors] = process.argv.slice(2);
let code = readFileSync(glue, "utf8");

function change(description, pattern, replacement, { atLeast = 1 } = {}) {
  let count = 0;
  code = code.replace(pattern, (...match) => {
    count += 1;
    return typeof replacement === "function" ? replacement(...match) : replacement;
  });
  if (count < atLeast) {
    console.error(`patch.mjs: ${description}: not found in ${glue}`);
    process.exit(1);
  }
}

change(
  "the import of the error classes",
  `import { makeError } from '${errors}';`,
  `import { makeError, SealwaxCrashedError } from '${errors}';`,
);
change(
  "objects stamped with their instance",
  /(\n\s*)obj\.__wbg_ptr = ptr;/g,
  (_, indent) => `${indent}obj.__wbg_ptr = ptr;${indent}obj.__sealwax_epoch = __sealwax_epoch;`,
);
change(
  "registrations naming their instance",
  /Finalization\.register\(obj, obj\.__wbg_ptr, obj\)/g,
  "Finalization.register(obj, [obj.__wbg_ptr, __sealwax_epoch], obj)",
);
change(
  "objects freed by the garbage collector in their own instance only",
  /new FinalizationRegistry\((\w+) => (wasm\.\w+)\(\1(?: >>> 0)?, 1\)\)/g,
  (_, _ptr, free) =>
    `new FinalizationRegistry(([ptr, epoch]) => { if (epoch === __sealwax_epoch && !__sealwax_crashed) ${free}(ptr >>> 0, 1); })`,
);
change(
  "pointers read through their instance",
  /this\.__wbg_ptr(?!\s*=[^=])/g,
  "__sealwax_pointer(this)",
);
change("the module's functions guarded", "wasm = instance.exports;", "wasm = __sealwax_guard(instance.exports);");

const cached = [...code.matchAll(/^let (cached\w+Memory0) = null;$/gm)].map((m) => m[1]);
if (cached.length === 0) {
  console.error("patch.mjs: no cached memory views found");
  process.exit(1);
}

code += `
let __sealwax_epoch = 0;
let __sealwax_crashed = false;

/** The pointer of an object of this instance; one from before a reset is refused. */
function __sealwax_pointer(object) {
    if (object.__sealwax_epoch !== __sealwax_epoch) {
        throw new SealwaxCrashedError(undefined, "a document opened before reset() was used: open it again");
    }
    if (__sealwax_crashed) {
        throw new SealwaxCrashedError();
    }
    return object.__wbg_ptr;
}

/** The module's exports, each function refused once the module has trapped. */
function __sealwax_guard(exports) {
    const guarded = {};
    for (const [name, value] of Object.entries(exports)) {
        if (typeof value !== "function") {
            guarded[name] = value;
            continue;
        }
        guarded[name] = function (...args) {
            if (__sealwax_crashed) {
                throw new SealwaxCrashedError();
            }
            try {
                return value.apply(this, args);
            } catch (error) {
                if (error instanceof WebAssembly.RuntimeError) {
                    __sealwax_crashed = true;
                    throw new SealwaxCrashedError(error);
                }
                throw error;
            }
        };
    }
    return guarded;
}

/** Whether the module has stopped, a panic aborting it: \`reset()\` makes another. */
export function __sealwax_is_crashed() {
    return __sealwax_crashed;
}

/** Drop the instance: the next initialization makes another, the objects of this one refused. */
export function __sealwax_forget() {
    wasm = undefined;
    wasmInstance = undefined;
    wasmModule = undefined;
${cached.map((name) => `    ${name} = null;`).join("\n")}
    __wbg_init_promise = undefined;
    __sealwax_epoch += 1;
    __sealwax_crashed = false;
}
`;
writeFileSync(glue, code);
