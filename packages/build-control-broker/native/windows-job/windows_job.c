// Windows Job Object containment for the managed build broker.
//
// UNTESTED ON THIS CHECKOUT: written on a Mac with no Windows host to compile
// or run against. It follows the standard, widely-documented Win32 pattern
// (the same one Chromium's sandbox launcher & Docker Desktop's Windows
// process host use) precisely, but it has never been built or exercised.
// Build with `node-gyp rebuild` on the target Windows machine and run
// windows_job.test.mjs before wiring it into process-tree.mjs's win32 branch.
//
// Why CREATE_SUSPENDED is required, not optional: AssignProcessToJobObject
// only captures processes already in the job when THEY spawn further
// children. If the launcher starts running before assignment completes, it
// could spawn a grandchild before the job owns it, and that grandchild would
// never be contained. Suspending the process at creation, assigning it to the
// job, then resuming closes that window completely — this is exactly what
// tools/rightkit/packages/build-control-broker/src/process-tree.mjs's
// WINDOWS_CONTAINMENT_CONTRACT requires.
#include <node_api.h>
#include <windows.h>
#include <stdlib.h>

static napi_value ThrowLastError(napi_env env, const char *what) {
  DWORD code = GetLastError();
  char message[256];
  wsprintfA(message, "%s failed with Win32 error %lu", what, code);
  napi_throw_error(env, NULL, message);
  return NULL;
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

static char *DupUtf16FromUtf8(napi_env env, napi_value value, int *outLen) {
  size_t utf8Len = 0;
  if (napi_get_value_string_utf8(env, value, NULL, 0, &utf8Len) != napi_ok) return NULL;
  char *utf8 = (char *)malloc(utf8Len + 1);
  if (!utf8) return NULL;
  size_t written = 0;
  napi_get_value_string_utf8(env, value, utf8, utf8Len + 1, &written);
  int wideLen = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, NULL, 0);
  WCHAR *wide = (WCHAR *)malloc(sizeof(WCHAR) * wideLen);
  if (wide) MultiByteToWideChar(CP_UTF8, 0, utf8, -1, wide, wideLen);
  free(utf8);
  if (outLen) *outLen = wideLen;
  return (char *)wide;
}

/**
 * spawnContained(commandLine: string, cwd: string | null) ->
 *   { pid: number, processHandle: external, jobHandle: external }
 *
 * commandLine must already be exactly what CreateProcessW expects (quoted
 * per Windows' own argv-splitting rules) — this function does no quoting or
 * escaping of its own, matching every other spawn boundary in this codebase
 * which builds argv explicitly rather than through a shell.
 */
static napi_value SpawnContained(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value args[2];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) {
    napi_throw_type_error(env, NULL, "spawnContained(commandLine, cwd) requires commandLine");
    return NULL;
  }

  WCHAR *commandLine = (WCHAR *)DupUtf16FromUtf8(env, args[0], NULL);
  if (!commandLine) { napi_throw_error(env, NULL, "failed to convert commandLine to UTF-16"); return NULL; }

  WCHAR *cwd = NULL;
  napi_valuetype cwdType;
  if (argc > 1 && napi_typeof(env, args[1], &cwdType) == napi_ok && cwdType == napi_string) {
    cwd = (WCHAR *)DupUtf16FromUtf8(env, args[1], NULL);
  }

  STARTUPINFOW startupInfo;
  PROCESS_INFORMATION processInfo;
  ZeroMemory(&startupInfo, sizeof(startupInfo));
  startupInfo.cb = sizeof(startupInfo);
  ZeroMemory(&processInfo, sizeof(processInfo));

  // CREATE_SUSPENDED: see the file header — this is the race the whole
  // function exists to close. CREATE_NO_WINDOW: this is a background broker
  // child; it must never flash a console.
  BOOL created = CreateProcessW(
    NULL, commandLine, NULL, NULL, FALSE,
    CREATE_SUSPENDED | CREATE_NO_WINDOW,
    NULL, cwd, &startupInfo, &processInfo);
  free(commandLine);
  if (cwd) free(cwd);
  if (!created) return ThrowLastError(env, "CreateProcessW");

  HANDLE job = CreateJobObjectW(NULL, NULL);
  if (!job) {
    TerminateProcess(processInfo.hProcess, 1);
    CloseHandle(processInfo.hThread);
    CloseHandle(processInfo.hProcess);
    return ThrowLastError(env, "CreateJobObjectW");
  }

  JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits;
  ZeroMemory(&limits, sizeof(limits));
  // KILL_ON_JOB_CLOSE: closing the last handle to this job terminates every
  // process in it — this is the broker's actual "reap the whole tree" primitive,
  // used instead of process-group signalling (which Windows has no equivalent
  // of). DIE_ON_UNHANDLED_EXCEPTION prevents a crashed child from lingering.
  limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
  if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
    CloseHandle(job);
    TerminateProcess(processInfo.hProcess, 1);
    CloseHandle(processInfo.hThread);
    CloseHandle(processInfo.hProcess);
    return ThrowLastError(env, "SetInformationJobObject");
  }

  // The process is still suspended here — nothing it does after this point,
  // including any child it spawns after resuming, can escape the job.
  if (!AssignProcessToJobObject(job, processInfo.hProcess)) {
    CloseHandle(job);
    TerminateProcess(processInfo.hProcess, 1);
    CloseHandle(processInfo.hThread);
    CloseHandle(processInfo.hProcess);
    return ThrowLastError(env, "AssignProcessToJobObject");
  }

  if (ResumeThread(processInfo.hThread) == (DWORD)-1) {
    CloseHandle(job);
    TerminateProcess(processInfo.hProcess, 1);
    CloseHandle(processInfo.hThread);
    CloseHandle(processInfo.hProcess);
    return ThrowLastError(env, "ResumeThread");
  }
  CloseHandle(processInfo.hThread);

  napi_value result, pidValue, processHandleValue, jobHandleValue;
  napi_create_object(env, &result);
  napi_create_uint32(env, processInfo.dwProcessId, &pidValue);
  processHandleValue = WrapHandle(env, processInfo.hProcess);
  jobHandleValue = WrapHandle(env, job);
  napi_set_named_property(env, result, "pid", pidValue);
  napi_set_named_property(env, result, "processHandle", processHandleValue);
  napi_set_named_property(env, result, "jobHandle", jobHandleValue);
  return result;
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
 * For a process Node's own spawn() already started (needed to keep Node's IPC
 * channel working — see the file header for spawnContained). This is
 * deliberately NOT the CREATE_SUSPENDED guarantee: the process has been
 * running since spawn() returned, so a child it forks in that window before
 * this call completes escapes the job. Node's spawn() returns as soon as
 * CreateProcess succeeds, before the child's own entry point runs any of its
 * own code, so that window is microseconds, not a meaningful gap for a
 * broker launcher whose first action is waiting on its parent's IPC message
 * — but it is a real, non-zero race, and callers must not represent this as
 * equivalent to spawnContained's airtight guarantee.
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

static napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor properties[] = {
    { "spawnContained", NULL, SpawnContained, NULL, NULL, NULL, napi_default, NULL },
    { "assignRunningProcessToNewJob", NULL, AssignRunningProcessToNewJob, NULL, NULL, NULL, napi_default, NULL },
    { "terminateJob", NULL, TerminateJob, NULL, NULL, NULL, napi_default, NULL },
    { "closeProcessHandle", NULL, CloseProcessHandleFn, NULL, NULL, NULL, napi_default, NULL },
    { "isProcessAlive", NULL, IsProcessAlive, NULL, NULL, NULL, napi_default, NULL },
    { "isPidAlive", NULL, IsPidAlive, NULL, NULL, NULL, napi_default, NULL },
  };
  napi_define_properties(env, exports, sizeof(properties) / sizeof(properties[0]), properties);
  return exports;
}

NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)
