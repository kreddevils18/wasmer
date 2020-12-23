// The Wasmer C/C++ header file compatible with the `wasm-c-api` standard API.
// This file is generated by lib/c-api/build.rs.

#if !defined(WASMER_WASM_H_PRELUDE)

#define WASMER_WASM_H_PRELUDE

// Define the `ARCH_X86_X64` constant.
#if defined(MSVC) && defined(_M_AMD64)
#  define ARCH_X86_64
#elif (defined(GCC) || defined(__GNUC__) || defined(__clang__)) && defined(__x86_64__)
#  define ARCH_X86_64
#endif

// Compatibility with non-Clang compilers.
#if !defined(__has_attribute)
#  define __has_attribute(x) 0
#endif

// Compatibility with non-Clang compilers.
#if !defined(__has_declspec_attribute)
#  define __has_declspec_attribute(x) 0
#endif

// Define the `DEPRECATED` macro.
#if defined(GCC) || defined(__GNUC__) || __has_attribute(deprecated)
#  define DEPRECATED(message) __attribute__((deprecated(message)))
#elif defined(MSVC) || __has_declspec_attribute(deprecated)
#  define DEPRECATED(message) __declspec(deprecated(message))
#endif

// The `jit` feature has been enabled for this build.
#define WASMER_JIT_ENABLED

// The `compiler` feature has been enabled for this build.
#define WASMER_COMPILER_ENABLED

// The `wasi` feature has been enabled for this build.
#define WASMER_WASI_ENABLED

// This file corresponds to the following Wasmer version.
#define WASMER_VERSION "1.0.0-rc1"
#define WASMER_VERSION_MAJOR 1
#define WASMER_VERSION_MINOR 0
#define WASMER_VERSION_PATCH 0
#define WASMER_VERSION_PRE "rc1"

#endif // WASMER_WASM_H_PRELUDE


//
// OK, here we go. The code below is automatically generated.
//


#ifndef WASMER_WASM_H
#define WASMER_WASM_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include "wasm.h"

#if defined(WASMER_COMPILER_ENABLED)
/**
 * Kind of compilers that can be used by the engines.
 *
 * This is a Wasmer-specific type with Wasmer-specific functions for
 * manipulating it.
 */
typedef enum {
  /**
   * Variant to represent the Cranelift compiler. See the
   * [`wasmer_compiler_cranelift`] Rust crate.
   */
  CRANELIFT = 0,
  /**
   * Variant to represent the LLVM compiler. See the
   * [`wasmer_compiler_llvm`] Rust crate.
   */
  LLVM = 1,
  /**
   * Variant to represent the Singlepass compiler. See the
   * [`wasmer_compiler_singlepass`] Rust crate.
   */
  SINGLEPASS = 2,
} wasmer_compiler_t;
#endif

/**
 * Kind of engines that can be used by the store.
 *
 * This is a Wasmer-specific type with Wasmer-specific functions for
 * manipulating it.
 */
typedef enum {
  /**
   * Variant to represent the JIT engine. See the
   * [`wasmer_engine_jit`] Rust crate.
   */
  JIT = 0,
  /**
   * Variant to represent the Native engine. See the
   * [`wasmer_engine_native`] Rust crate.
   */
  NATIVE = 1,
  /**
   * Variant to represent the Object File engine. See the
   * [`wasmer_engine_object_file`] Rust crate.
   */
  OBJECT_FILE = 2,
} wasmer_engine_t;

#if defined(WASMER_WASI_ENABLED)
typedef struct wasi_config_t wasi_config_t;
#endif

#if defined(WASMER_WASI_ENABLED)
typedef struct wasi_env_t wasi_env_t;
#endif

#if defined(WASMER_WASI_ENABLED)
typedef struct wasi_version_t wasi_version_t;
#endif

#if defined(WASMER_WASI_ENABLED)
void wasi_config_arg(wasi_config_t *config, const char *arg);
#endif

#if defined(WASMER_WASI_ENABLED)
void wasi_config_env(wasi_config_t *config, const char *key, const char *value);
#endif

#if defined(WASMER_WASI_ENABLED)
void wasi_config_inherit_stderr(wasi_config_t *config);
#endif

#if defined(WASMER_WASI_ENABLED)
void wasi_config_inherit_stdin(wasi_config_t *config);
#endif

#if defined(WASMER_WASI_ENABLED)
void wasi_config_inherit_stdout(wasi_config_t *config);
#endif

#if defined(WASMER_WASI_ENABLED)
bool wasi_config_mapdir(wasi_config_t *config, const char *alias, const char *dir);
#endif

#if defined(WASMER_WASI_ENABLED)
wasi_config_t *wasi_config_new(const char *program_name);
#endif

#if defined(WASMER_WASI_ENABLED)
bool wasi_config_preopen_dir(wasi_config_t *config, const char *dir);
#endif

#if defined(WASMER_WASI_ENABLED)
void wasi_env_delete(wasi_env_t *_state);
#endif

#if defined(WASMER_WASI_ENABLED)
/**
 * Takes ownership over the `wasi_config_t`.
 */
wasi_env_t *wasi_env_new(wasi_config_t *config);
#endif

#if defined(WASMER_WASI_ENABLED)
intptr_t wasi_env_read_stderr(wasi_env_t *env, char *buffer, uintptr_t buffer_len);
#endif

#if defined(WASMER_WASI_ENABLED)
intptr_t wasi_env_read_stdout(wasi_env_t *env, char *buffer, uintptr_t buffer_len);
#endif

#if defined(WASMER_WASI_ENABLED)
/**
 * This function is deprecated. You may safely remove all calls to it and everything
 * will continue to work.
 */
bool wasi_env_set_instance(wasi_env_t *env, const wasm_instance_t *instance);
#endif

#if defined(WASMER_WASI_ENABLED)
/**
 * This function is deprecated. You may safely remove all calls to it and everything
 * will continue to work.
 */
void wasi_env_set_memory(wasi_env_t *env, const wasm_memory_t *memory);
#endif

#if defined(WASMER_WASI_ENABLED)
/**
 * Takes ownership of `wasi_env_t`.
 */
bool wasi_get_imports(const wasm_store_t *store,
                      const wasm_module_t *module,
                      const wasi_env_t *wasi_env,
                      wasm_extern_vec_t *imports);
#endif

#if defined(WASMER_WASI_ENABLED)
wasm_func_t *wasi_get_start_function(wasm_instance_t *instance);
#endif

#if defined(WASMER_WASI_ENABLED)
wasi_version_t wasi_get_wasi_version(const wasm_module_t *module);
#endif

#if defined(WASMER_COMPILER_ENABLED)
/**
 * Updates the configuration to specify a particular compiler to use.
 *
 * This is a Wasmer-specific function.
 *
 * # Example
 *
 * ```rust,no_run
 * # use inline_c::assert_c;
 * # fn main() {
 * #    (assert_c! {
 * # #include "tests/wasmer_wasm.h"
 * #
 * int main() {
 *     // Create the configuration.
 *     wasm_config_t* config = wasm_config_new();
 *
 *     // Use the Cranelift compiler.
 *     wasm_config_set_compiler(config, CRANELIFT);
 *
 *     // Create the engine.
 *     wasm_engine_t* engine = wasm_engine_new_with_config(config);
 *
 *     // Check we have an engine!
 *     assert(engine);
 *
 *     // Free everything.
 *     wasm_engine_delete(engine);
 *
 *     return 0;
 * }
 * #    })
 * #    .success();
 * # }
 * ```
 */
void wasm_config_set_compiler(wasm_config_t *config, wasmer_compiler_t compiler);
#endif

/**
 * Updates the configuration to specify a particular engine to use.
 *
 * This is a Wasmer-specific function.
 *
 * # Example
 *
 * ```rust,no_run
 * # use inline_c::assert_c;
 * # fn main() {
 * #    (assert_c! {
 * # #include "tests/wasmer_wasm.h"
 * #
 * int main() {
 *     // Create the configuration.
 *     wasm_config_t* config = wasm_config_new();
 *
 *     // Use the JIT engine.
 *     wasm_config_set_engine(config, JIT);
 *
 *     // Create the engine.
 *     wasm_engine_t* engine = wasm_engine_new_with_config(config);
 *
 *     // Check we have an engine!
 *     assert(engine);
 *
 *     // Free everything.
 *     wasm_engine_delete(engine);
 *
 *     return 0;
 * }
 * #    })
 * #    .success();
 * # }
 * ```
 */
void wasm_config_set_engine(wasm_config_t *config, wasmer_engine_t engine);

/**
 * Non-standard Wasmer-specific API to get the module's name,
 * otherwise `out->size` is set to `0` and `out->data` to `NULL`.
 *
 * # Example
 *
 * ```rust
 * # use inline_c::assert_c;
 * # fn main() {
 * #    (assert_c! {
 * # #include "tests/wasmer_wasm.h"
 * #
 * int main() {
 *     // Create the engine and the store.
 *     wasm_engine_t* engine = wasm_engine_new();
 *     wasm_store_t* store = wasm_store_new(engine);
 *
 *     // Create a WebAssembly module from a WAT definition.
 *     wasm_byte_vec_t wat;
 *     wasmer_byte_vec_new_from_string(&wat, "(module $moduleName)");
 *     //                                             ^~~~~~~~~~~ that's the name!
 *     wasm_byte_vec_t wasm;
 *     wat2wasm(&wat, &wasm);
 *
 *     // Create the module.
 *     wasm_module_t* module = wasm_module_new(store, &wasm);
 *
 *     // Read the module's name.
 *     wasm_name_t name;
 *     wasm_module_name(module, &name);
 *
 *     // It works!
 *     wasmer_assert_name(&name, "moduleName");
 *
 *     // Free everything.
 *     wasm_byte_vec_delete(&name);
 *     wasm_module_delete(module);
 *     wasm_byte_vec_delete(&wasm);
 *     wasm_byte_vec_delete(&wat);
 *     wasm_store_delete(store);
 *     wasm_engine_delete(engine);
 *
 *     return 0;
 * }
 * #    })
 * #    .success();
 * # }
 * ```
 */
void wasm_module_name(const wasm_module_t *module, wasm_name_t *out);

/**
 * Non-standard Wasmer-specific API to set the module's name. The
 * function returns `true` if the name has been updated, `false`
 * otherwise.
 *
 * # Example
 *
 * ```rust
 * # use inline_c::assert_c;
 * # fn main() {
 * #    (assert_c! {
 * # #include "tests/wasmer_wasm.h"
 * #
 * int main() {
 *     // Create the engine and the store.
 *     wasm_engine_t* engine = wasm_engine_new();
 *     wasm_store_t* store = wasm_store_new(engine);
 *
 *     // Create a WebAssembly module from a WAT definition.
 *     wasm_byte_vec_t wat;
 *     wasmer_byte_vec_new_from_string(&wat, "(module)");
 *     wasm_byte_vec_t wasm;
 *     wat2wasm(&wat, &wasm);
 *
 *     // Create the module.
 *     wasm_module_t* module = wasm_module_new(store, &wasm);
 *
 *     // Read the module's name. There is none for the moment.
 *     {
 *         wasm_name_t name;
 *         wasm_module_name(module, &name);
 *
 *         assert(name.size == 0);
 *     }
 *
 *     // So, let's set a new name.
 *     {
 *         wasm_name_t name;
 *         wasmer_byte_vec_new_from_string(&name, "hello");
 *         wasm_module_set_name(module, &name);
 *     }
 *
 *     // And now, let's see the new name.
 *     {
 *         wasm_name_t name;
 *         wasm_module_name(module, &name);
 *
 *         // It works!
 *         wasmer_assert_name(&name, "hello");
 *
 *         wasm_byte_vec_delete(&name);
 *     }
 *
 *     // Free everything.
 *     wasm_module_delete(module);
 *     wasm_byte_vec_delete(&wasm);
 *     wasm_byte_vec_delete(&wat);
 *     wasm_store_delete(store);
 *     wasm_engine_delete(engine);
 *
 *     return 0;
 * }
 * #    })
 * #    .success();
 * # }
 * ```
 */
bool wasm_module_set_name(wasm_module_t *module, const wasm_name_t *name);

/**
 * Gets the length in bytes of the last error if any, zero otherwise.
 *
 * This can be used to dynamically allocate a buffer with the correct number of
 * bytes needed to store a message.
 *
 * # Example
 *
 * See this module's documentation to get a complete example.
 */
int wasmer_last_error_length(void);

/**
 * Gets the last error message if any into the provided buffer
 * `buffer` up to the given `length`.
 *
 * The `length` parameter must be large enough to store the last
 * error message. Ideally, the value should come from
 * [`wasmer_last_error_length`].
 *
 * The function returns the length of the string in bytes, `-1` if an
 * error occurs. Potential errors are:
 *
 *  * The `buffer` is a null pointer,
 *  * The `buffer` is too small to hold the error message.
 *
 * Note: The error message always has a trailing NUL character.
 *
 * Important note: If the provided `buffer` is non-null, once this
 * function has been called, regardless whether it fails or succeeds,
 * the error is cleared.
 *
 * # Example
 *
 * See this module's documentation to get a complete example.
 */
int wasmer_last_error_message(char *buffer, int length);

/**
 * Get the version of the Wasmer C API.
 *
 * The `.h` files already define variables like `WASMER_VERSION*`,
 * but if this file is unreachable, one can use this function to
 * retrieve the full semver version of the Wasmer C API.
 *
 * The returned string is statically allocated. It must _not_ be
 * freed!
 *
 * # Example
 *
 * See the module's documentation.
 */
const char *wasmer_version(void);

/**
 * Get the major version of the Wasmer C API.
 *
 * See [`wasmer_version`] to learn more.
 *
 * # Example
 *
 * ```rust
 * # use inline_c::assert_c;
 * # fn main() {
 * #    (assert_c! {
 * # #include "tests/wasmer_wasm.h"
 * #
 * int main() {
 *     // Get and print the version components.
 *     uint8_t version_major = wasmer_version_major();
 *     uint8_t version_minor = wasmer_version_minor();
 *     uint8_t version_patch = wasmer_version_patch();
 *
 *     printf("%d.%d.%d", version_major, version_minor, version_patch);
 *
 *     return 0;
 * }
 * #    })
 * #    .success()
 * #    .stdout(
 * #         format!(
 * #             "{}.{}.{}",
 * #             env!("CARGO_PKG_VERSION_MAJOR"),
 * #             env!("CARGO_PKG_VERSION_MINOR"),
 * #             env!("CARGO_PKG_VERSION_PATCH")
 * #         )
 * #     );
 * # }
 * ```
 */
uint8_t wasmer_version_major(void);

/**
 * Get the minor version of the Wasmer C API.
 *
 * See [`wasmer_version_major`] to learn more and get an example.
 */
uint8_t wasmer_version_minor(void);

/**
 * Get the patch version of the Wasmer C API.
 *
 * See [`wasmer_version_major`] to learn more and get an example.
 */
uint8_t wasmer_version_patch(void);

/**
 * Get the minor version of the Wasmer C API.
 *
 * See [`wasmer_version_major`] to learn more.
 *
 * The returned string is statically allocated. It must _not_ be
 * freed!
 *
 * # Example
 *
 * ```rust
 * # use inline_c::assert_c;
 * # fn main() {
 * #    (assert_c! {
 * # #include "tests/wasmer_wasm.h"
 * #
 * int main() {
 *     // Get and print the pre version.
 *     const char* version_pre = wasmer_version_pre();
 *     printf("%s", version_pre);
 *
 *     // No need to free the string. It's statically allocated on
 *     // the Rust side.
 *
 *     return 0;
 * }
 * #    })
 * #    .success()
 * #    .stdout(env!("CARGO_PKG_VERSION_PRE"));
 * # }
 * ```
 */
const char *wasmer_version_pre(void);

/**
 * Parses in-memory bytes as either the WAT format, or a binary Wasm
 * module. This is wasmer-specific.
 *
 * In case of failure, `wat2wasm` sets the `out->data = NULL` and `out->size = 0`.
 *
 * # Example
 *
 * See the module's documentation.
 */
void wat2wasm(const wasm_byte_vec_t *wat, wasm_byte_vec_t *out);

#endif /* WASMER_WASM_H */
