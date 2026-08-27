// Windows Job Object containment for the managed build broker.
//
// Built and exercised on the native Windows host on 2026-08-22. Acceptance
// covered successful compile, explicit cancellation, broker-crash recovery,
// creation-time identity, whole-tree reap, and generation rotation.
//
// Node owns launcher creation so it can also own stdout, stderr, close events,
// & IPC. Managed launcher cannot spawn payload until broker sends IPC START.
// Broker assigns launcher synchronously to KILL_ON_JOB_CLOSE Job Object first,
// persists exact creation-time identity, then sends START. Every managed tool
// process & descendant therefore begins inside Job Object.
#include <node_api.h>
#include <windows.h>
#include <aclapi.h>

static napi_value ThrowLastError(napi_env env, const char *what) {
  DWORD code = GetLastError();
  char message[256];
  wsprintfA(message, "%s failed with Win32 error %lu", what, code);
  napi_throw_error(env, NULL, message);
  return NULL;
}

static napi_value ThrowWin32Error(napi_env env, const char *what, DWORD code) {
  char message[256];
  wsprintfA(message, "%s failed with Win32 error %lu", what, code);
  napi_throw_error(env, NULL, message);
  return NULL;
}

/** Grant this process token's exact user SID duplex access in its default
 * DACL before libuv creates the broker pipe. Task Scheduler S4U tokens for
 * local admins otherwise grant pipe writes only to an admin SID that medium
 * interactive tokens carry as deny-only. Existing ACEs remain unchanged. */
static napi_value GrantCurrentUserDefaultDacl(napi_env env, napi_callback_info info) {
  HANDLE token = NULL;
  BYTE *userBuffer = NULL;
  BYTE *daclBuffer = NULL;
  PACL updatedDacl = NULL;
  napi_value undefined;

  if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_ADJUST_DEFAULT, &token))
    return ThrowLastError(env, "OpenProcessToken");

  DWORD userBytes = 0;
  GetTokenInformation(token, TokenUser, NULL, 0, &userBytes);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) {
    CloseHandle(token);
    return ThrowLastError(env, "GetTokenInformation(TokenUser size)");
  }
  userBuffer = (BYTE *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, userBytes);
  if (!userBuffer) { CloseHandle(token); return ThrowWin32Error(env, "HeapAlloc(TokenUser)", ERROR_OUTOFMEMORY); }
  if (!GetTokenInformation(token, TokenUser, userBuffer, userBytes, &userBytes)) {
    HeapFree(GetProcessHeap(), 0, userBuffer); CloseHandle(token);
    return ThrowLastError(env, "GetTokenInformation(TokenUser)");
  }

  DWORD daclBytes = 0;
  GetTokenInformation(token, TokenDefaultDacl, NULL, 0, &daclBytes);
  if (GetLastError() != ERROR_INSUFFICIENT_BUFFER) {
    HeapFree(GetProcessHeap(), 0, userBuffer); CloseHandle(token);
    return ThrowLastError(env, "GetTokenInformation(TokenDefaultDacl size)");
  }
  daclBuffer = (BYTE *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, daclBytes);
  if (!daclBuffer) {
    HeapFree(GetProcessHeap(), 0, userBuffer); CloseHandle(token);
    return ThrowWin32Error(env, "HeapAlloc(TokenDefaultDacl)", ERROR_OUTOFMEMORY);
  }
  if (!GetTokenInformation(token, TokenDefaultDacl, daclBuffer, daclBytes, &daclBytes)) {
    HeapFree(GetProcessHeap(), 0, daclBuffer); HeapFree(GetProcessHeap(), 0, userBuffer); CloseHandle(token);
    return ThrowLastError(env, "GetTokenInformation(TokenDefaultDacl)");
  }

  TOKEN_USER *tokenUser = (TOKEN_USER *)userBuffer;
  TOKEN_DEFAULT_DACL *current = (TOKEN_DEFAULT_DACL *)daclBuffer;
  EXPLICIT_ACCESSW access;
  ZeroMemory(&access, sizeof(access));
  access.grfAccessPermissions = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
  access.grfAccessMode = GRANT_ACCESS;
  access.grfInheritance = NO_INHERITANCE;
  access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
  access.Trustee.TrusteeType = TRUSTEE_IS_USER;
  access.Trustee.ptstrName = (LPWSTR)tokenUser->User.Sid;

  DWORD aclError = SetEntriesInAclW(1, &access, current->DefaultDacl, &updatedDacl);
  if (aclError != ERROR_SUCCESS) {
    HeapFree(GetProcessHeap(), 0, daclBuffer); HeapFree(GetProcessHeap(), 0, userBuffer); CloseHandle(token);
    return ThrowWin32Error(env, "SetEntriesInAclW", aclError);
  }

  TOKEN_DEFAULT_DACL replacement;
  replacement.DefaultDacl = updatedDacl;
  BOOL changed = SetTokenInformation(token, TokenDefaultDacl, &replacement, sizeof(replacement));
  DWORD changeError = changed ? ERROR_SUCCESS : GetLastError();
  LocalFree(updatedDacl);
  HeapFree(GetProcessHeap(), 0, daclBuffer);
  HeapFree(GetProcessHeap(), 0, userBuffer);
  CloseHandle(token);
  if (!changed) return ThrowWin32Error(env, "SetTokenInformation(TokenDefaultDacl)", changeError);

  napi_get_undefined(env, &undefined);
  return undefined;
}

static napi_value WrapHandle(napi_env env, HANDLE handle) {
  napi_value external;
  // HANDLE is a fixed-size opaque value; wrapping it in napi_external keeps
  // it entirely out of JS's reach — no numeric handle value a script could
  // forge or corrupt.
  napi_status status = napi_create_external(env, handle, NULL, NULL, &external);
  if (status != napi_ok) { napi_throw_error(env, NULL, "napi_create_external failed"); return NULL; }
  return external;
}

static HANDLE UnwrapHandle(napi_env env, napi_value value) {
  void *data = NULL;
  if (napi_get_value_external(env, value, &data) != napi_ok) {
    napi_throw_type_error(env, NULL, "expected a native handle");
    return NULL;
  }
  return (HANDLE)data;
}

/** terminateJob(jobHandle: external) -> void
 * TerminateJobObject first (explicit, immediate), then close the handle —
 * relying on KILL_ON_JOB_CLOSE alone would still work, but calling
 * TerminateJobObject directly does not wait for GC-driven handle cleanup. */
static napi_value TerminateJob(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "terminateJob(jobHandle) requires jobHandle");
    return NULL;
  }
  HANDLE job = UnwrapHandle(env, args[0]);
  if (!job) return NULL;
  TerminateJobObject(job, 1);
  CloseHandle(job);
  napi_value undefined;
  napi_get_undefined(env, &undefined);
  return undefined;
}

/** closeProcessHandle(processHandle: external) -> void */
static napi_value CloseProcessHandleFn(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "closeProcessHandle(processHandle) requires processHandle");
    return NULL;
  }
  HANDLE process = UnwrapHandle(env, args[0]);
  if (!process) return NULL;
  CloseHandle(process);
  napi_value undefined;
  napi_get_undefined(env, &undefined);
  return undefined;
}

/** isProcessAlive(processHandle: external) -> boolean
 * A held handle keeps the PID from being reused by an unrelated process for
 * the lifetime of that handle, so this check — unlike a bare PID liveness
 * probe — cannot false-positive on PID reuse. */
static napi_value IsProcessAlive(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "isProcessAlive(processHandle) requires processHandle");
    return NULL;
  }
  HANDLE process = UnwrapHandle(env, args[0]);
  if (!process) return NULL;
  DWORD exitCode = 0;
  napi_value result;
  if (!GetExitCodeProcess(process, &exitCode)) return ThrowLastError(env, "GetExitCodeProcess");
  napi_get_boolean(env, exitCode == STILL_ACTIVE, &result);
  return result;
}

/**
 * assignRunningProcessToNewJob(pid: number) -> { processHandle, jobHandle }
 *
 * Node owns this launcher's IPC channel. Its pinned implementation waits at
 * IPC START & cannot create payload before broker assigns Job Object. Caller
 * must assign synchronously, persist ownership, then send START.
 */
static napi_value AssignRunningProcessToNewJob(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "assignRunningProcessToNewJob(pid) requires pid");
    return NULL;
  }
  uint32_t pid = 0;
  if (napi_get_value_uint32(env, args[0], &pid) != napi_ok) {
    napi_throw_type_error(env, NULL, "pid must be a uint32");
    return NULL;
  }
  HANDLE process = OpenProcess(PROCESS_TERMINATE | PROCESS_SET_QUOTA | PROCESS_QUERY_INFORMATION | SYNCHRONIZE, FALSE, pid);
  if (!process) return ThrowLastError(env, "OpenProcess");

  HANDLE job = CreateJobObjectW(NULL, NULL);
  if (!job) { CloseHandle(process); return ThrowLastError(env, "CreateJobObjectW"); }

  JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits;
  ZeroMemory(&limits, sizeof(limits));
  limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
  if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
    CloseHandle(job); CloseHandle(process);
    return ThrowLastError(env, "SetInformationJobObject");
  }
  if (!AssignProcessToJobObject(job, process)) {
    CloseHandle(job); CloseHandle(process);
    return ThrowLastError(env, "AssignProcessToJobObject");
  }

  napi_value result, processHandleValue, jobHandleValue;
  napi_create_object(env, &result);
  processHandleValue = WrapHandle(env, process);
  jobHandleValue = WrapHandle(env, job);
  napi_set_named_property(env, result, "processHandle", processHandleValue);
  napi_set_named_property(env, result, "jobHandle", jobHandleValue);
  return result;
}

/** isPidAlive(pid: number) -> boolean
 * Used only for cross-restart reap, where no live handle from the process
 * that originally spawned this PID exists — see process-tree.mjs's
 * reapPersistedProcessTree for why this path is rarely reached at all: an
 * anonymous job with KILL_ON_JOB_CLOSE is torn down by the OS itself the
 * instant the owning broker process exits, handle count reaching zero,
 * whether that exit was graceful or a crash. */
static napi_value IsPidAlive(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "isPidAlive(pid) requires pid");
    return NULL;
  }
  uint32_t pid = 0;
  if (napi_get_value_uint32(env, args[0], &pid) != napi_ok) {
    napi_throw_type_error(env, NULL, "pid must be a uint32");
    return NULL;
  }
  napi_value result;
  HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
  if (!process) { napi_get_boolean(env, FALSE, &result); return result; }
  DWORD exitCode = 0;
  BOOL ok = GetExitCodeProcess(process, &exitCode);
  CloseHandle(process);
  napi_get_boolean(env, ok && exitCode == STILL_ACTIVE, &result);
  return result;
}

/** getProcessCreationTime(pid: number) -> string
 * Returns the kernel creation FILETIME as a decimal string. Unlike a PID,
 * this token changes on reuse. Reading it through the already-required native
 * addon avoids spawning PowerShell on every managed process admission. */
static napi_value GetProcessCreationTimeFn(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "getProcessCreationTime(pid) requires pid");
    return NULL;
  }
  uint32_t pid = 0;
  if (napi_get_value_uint32(env, args[0], &pid) != napi_ok) {
    napi_throw_type_error(env, NULL, "pid must be a uint32");
    return NULL;
  }
  HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
  if (!process) return ThrowLastError(env, "OpenProcess");
  FILETIME created, exited, kernel, user;
  BOOL ok = GetProcessTimes(process, &created, &exited, &kernel, &user);
  CloseHandle(process);
  if (!ok) return ThrowLastError(env, "GetProcessTimes");
  ULARGE_INTEGER value;
  value.LowPart = created.dwLowDateTime;
  value.HighPart = created.dwHighDateTime;
  char token[32];
  _ui64toa_s(value.QuadPart, token, sizeof(token), 10);
  napi_value result;
  napi_create_string_utf8(env, token, NAPI_AUTO_LENGTH, &result);
  return result;
}

static napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor properties[] = {
    { "grantCurrentUserDefaultDacl", NULL, GrantCurrentUserDefaultDacl, NULL, NULL, NULL, napi_default, NULL },
    { "assignRunningProcessToNewJob", NULL, AssignRunningProcessToNewJob, NULL, NULL, NULL, napi_default, NULL },
    { "terminateJob", NULL, TerminateJob, NULL, NULL, NULL, napi_default, NULL },
    { "closeProcessHandle", NULL, CloseProcessHandleFn, NULL, NULL, NULL, napi_default, NULL },
    { "isProcessAlive", NULL, IsProcessAlive, NULL, NULL, NULL, napi_default, NULL },
    { "isPidAlive", NULL, IsPidAlive, NULL, NULL, NULL, napi_default, NULL },
    { "getProcessCreationTime", NULL, GetProcessCreationTimeFn, NULL, NULL, NULL, napi_default, NULL },
  };
  napi_define_properties(env, exports, sizeof(properties) / sizeof(properties[0]), properties);
  return exports;
}

NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)
