#include <node_api.h>
#include <libproc.h>
#include <sys/proc_info.h>
#include <sys/proc.h>
#include <sys/resource.h>
#include <unistd.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
  pid_t pid;
  uint64_t start;
  uint64_t user_ns;
  uint64_t system_ns;
  bool present;
} process_sample;

typedef struct {
  pid_t root_pid;
  uint32_t root_pgid;
  bool root_pgid_set;
  uint64_t root_start;
  bool root_start_set;
  process_sample *samples;
  size_t sample_count;
  size_t sample_capacity;
  uint64_t raw_cpu_user_ns;
  uint64_t raw_cpu_system_ns;
  uint64_t children_baseline_user_ns;
  uint64_t children_baseline_system_ns;
  uint64_t children_user_ns;
  uint64_t children_system_ns;
  bool children_baseline_set;
  uint64_t cpu_user_ns;
  uint64_t cpu_system_ns;
  uint64_t peak_memory;
  uint32_t active_processes;
  uint32_t total_processes;
} resource_tracker;

static napi_value throw_error(napi_env env, const char *message) {
  napi_throw_error(env, NULL, message);
  return NULL;
}

static bool read_pid(napi_env env, napi_value value, pid_t *pid) {
  int32_t parsed = 0;
  if (napi_get_value_int32(env, value, &parsed) != napi_ok || parsed < 1) {
    napi_throw_type_error(env, NULL, "pid must be a positive integer");
    return false;
  }
  *pid = (pid_t)parsed;
  return true;
}

static bool read_bsd(pid_t pid, struct proc_bsdinfo *info) {
  memset(info, 0, sizeof(*info));
  return proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info, sizeof(*info)) == (int)sizeof(*info);
}

static uint64_t process_start(const struct proc_bsdinfo *info) {
  return ((uint64_t)info->pbi_start_tvsec * 1000000ULL) + (uint64_t)info->pbi_start_tvusec;
}

static process_sample *find_sample(resource_tracker *tracker, pid_t pid, uint64_t start) {
  for (size_t index = 0; index < tracker->sample_count; index++) {
    process_sample *sample = &tracker->samples[index];
    if (sample->pid == pid && sample->start == start) return sample;
  }
  return NULL;
}

static process_sample *add_sample(resource_tracker *tracker, pid_t pid, uint64_t start) {
  if (tracker->sample_count == tracker->sample_capacity) {
    size_t capacity = tracker->sample_capacity ? tracker->sample_capacity * 2 : 32;
    process_sample *next = realloc(tracker->samples, capacity * sizeof(*next));
    if (!next) return NULL;
    tracker->samples = next;
    tracker->sample_capacity = capacity;
  }
  process_sample *sample = &tracker->samples[tracker->sample_count++];
  memset(sample, 0, sizeof(*sample));
  sample->pid = pid;
  sample->start = start;
  return sample;
}

static const struct proc_bsdinfo *find_group_info(const struct proc_bsdinfo *infos, size_t count, pid_t pid) {
  for (size_t index = 0; index < count; index++) {
    if ((pid_t)infos[index].pbi_pid == pid) return &infos[index];
  }
  return NULL;
}

static bool is_group_descendant(pid_t root, const struct proc_bsdinfo *candidate, const struct proc_bsdinfo *infos, size_t count) {
  if ((pid_t)candidate->pbi_pid == root) return true;
  pid_t current = (pid_t)candidate->pbi_ppid;
  for (unsigned int depth = 0; depth < 1024 && current > 1; depth++) {
    if (current == root) return true;
    const struct proc_bsdinfo *parent = find_group_info(infos, count, current);
    if (!parent || (pid_t)parent->pbi_ppid == current) return false;
    current = (pid_t)parent->pbi_ppid;
  }
  return false;
}

static uint64_t ns_to_ms(uint64_t value) { return value / 1000000ULL; }

static uint64_t timeval_to_ns(const struct timeval *value) {
  if (!value || value->tv_sec < 0 || value->tv_usec < 0) return 0;
  return ((uint64_t)value->tv_sec * 1000000000ULL) + ((uint64_t)value->tv_usec * 1000ULL);
}

static bool read_children_usage(uint64_t *user_ns, uint64_t *system_ns) {
  struct rusage usage;
  memset(&usage, 0, sizeof(usage));
  if (getrusage(RUSAGE_CHILDREN, &usage) != 0) return false;
  *user_ns = timeval_to_ns(&usage.ru_utime);
  *system_ns = timeval_to_ns(&usage.ru_stime);
  return true;
}

static bool sample_process(resource_tracker *tracker, pid_t pid, uint64_t start, uint64_t *memory) {
  struct rusage_info_v4 usage;
  memset(&usage, 0, sizeof(usage));
  if (proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&usage) != 0) return false;
  process_sample *sample = find_sample(tracker, pid, start);
  if (!sample) {
    sample = add_sample(tracker, pid, start);
    if (!sample) return false;
    tracker->total_processes += 1;
  }
  const uint64_t user = usage.ri_user_time;
  const uint64_t system = usage.ri_system_time;
  const bool was_present = sample->present;
  const uint64_t previous_user = sample->user_ns;
  const uint64_t previous_system = sample->system_ns;
  if (was_present) {
    if (user >= previous_user) tracker->raw_cpu_user_ns += user - previous_user;
    if (system >= previous_system) tracker->raw_cpu_system_ns += system - previous_system;
  } else {
    tracker->raw_cpu_user_ns += user;
    tracker->raw_cpu_system_ns += system;
    sample->present = true;
  }
  sample->user_ns = user;
  sample->system_ns = system;
  *memory += usage.ri_phys_footprint;
  return true;
}

static void initialize_children_baseline(resource_tracker *tracker) {
  uint64_t user = 0, system = 0;
  if (read_children_usage(&user, &system)) {
    tracker->children_baseline_user_ns = user;
    tracker->children_baseline_system_ns = system;
    tracker->children_baseline_set = true;
  }
}

static void reconcile_cpu(resource_tracker *tracker) {
  uint64_t children_user = 0, children_system = 0;
  if (read_children_usage(&children_user, &children_system)) {
    if (!tracker->children_baseline_set) {
      tracker->children_baseline_user_ns = children_user;
      tracker->children_baseline_system_ns = children_system;
      tracker->children_baseline_set = true;
    }
    tracker->children_user_ns = children_user >= tracker->children_baseline_user_ns ? children_user - tracker->children_baseline_user_ns : 0;
    tracker->children_system_ns = children_system >= tracker->children_baseline_system_ns ? children_system - tracker->children_baseline_system_ns : 0;
  }
  // Darwin RUSAGE_CHILDREN includes a waited child's waited descendants. It
  // can therefore overlap live libproc samples; max keeps the two views from
  // double-counting while filling gaps for short-lived processes.
  tracker->cpu_user_ns = tracker->raw_cpu_user_ns > tracker->children_user_ns ? tracker->raw_cpu_user_ns : tracker->children_user_ns;
  tracker->cpu_system_ns = tracker->raw_cpu_system_ns > tracker->children_system_ns ? tracker->raw_cpu_system_ns : tracker->children_system_ns;
}

static bool update_tracker(resource_tracker *tracker) {
  if (!tracker->root_pgid_set) {
    struct proc_bsdinfo root_info;
    if (!read_bsd(tracker->root_pid, &root_info)) return false;
    tracker->root_pgid = root_info.pbi_pgid;
    tracker->root_pgid_set = true;
  }
  int bytes = proc_listpids(PROC_PGRP_ONLY, tracker->root_pgid, NULL, 0);
  if (bytes <= 0) {
    struct proc_bsdinfo root_info;
    if (read_bsd(tracker->root_pid, &root_info)) return false;
    tracker->active_processes = 0;
    reconcile_cpu(tracker);
    return true;
  }
  pid_t *pids = calloc((size_t)bytes, sizeof(*pids));
  if (!pids) return false;
  int pid_bytes = proc_listpids(PROC_PGRP_ONLY, tracker->root_pgid, pids, bytes);
  if (pid_bytes <= 0) { free(pids); return false; }
  const size_t pid_count = (size_t)pid_bytes / sizeof(*pids);
  struct proc_bsdinfo *infos = calloc(pid_count, sizeof(*infos));
  if (!infos) { free(pids); return false; }
  size_t info_count = 0;
  for (size_t index = 0; index < pid_count; index++) {
    if (read_bsd(pids[index], &infos[info_count])) info_count += 1;
  }
  tracker->active_processes = 0;
  uint64_t memory = 0;
  for (size_t index = 0; index < info_count; index++) {
    const struct proc_bsdinfo *info = &infos[index];
    const pid_t pid = (pid_t)info->pbi_pid;
    if (!is_group_descendant(tracker->root_pid, info, infos, info_count)) continue;
    const uint64_t start = process_start(info);
    if (pid == tracker->root_pid) {
      if (tracker->root_start_set && tracker->root_start != start) { free(infos); free(pids); return false; }
      tracker->root_start = start;
      tracker->root_start_set = true;
    }
    if (sample_process(tracker, pid, start, &memory) && info->pbi_status != SZOMB) tracker->active_processes += 1;
  }
  reconcile_cpu(tracker);
  if (memory > tracker->peak_memory) tracker->peak_memory = memory;
  free(infos);
  free(pids);
  return true;
}

static napi_value snapshot_value(napi_env env, resource_tracker *tracker) {
  napi_value result, value;
  napi_create_object(env, &result);
  napi_create_string_utf8(env, "OK", NAPI_AUTO_LENGTH, &value); napi_set_named_property(env, result, "status", value);
  napi_create_double(env, (double)ns_to_ms(tracker->cpu_user_ns), &value); napi_set_named_property(env, result, "cpuUserMs", value);
  napi_create_double(env, (double)ns_to_ms(tracker->cpu_system_ns), &value); napi_set_named_property(env, result, "cpuKernelMs", value);
  napi_create_double(env, (double)ns_to_ms(tracker->cpu_user_ns + tracker->cpu_system_ns), &value); napi_set_named_property(env, result, "cpuTotalMs", value);
  napi_create_double(env, (double)tracker->peak_memory, &value); napi_set_named_property(env, result, "peakMemoryBytes", value);
  napi_create_string_utf8(env, "physical-footprint", NAPI_AUTO_LENGTH, &value); napi_set_named_property(env, result, "memoryKind", value);
  napi_create_string_utf8(env, "sampled-physical-footprint-peak", NAPI_AUTO_LENGTH, &value); napi_set_named_property(env, result, "memorySemantics", value);
  napi_create_uint32(env, tracker->total_processes, &value); napi_set_named_property(env, result, "totalProcesses", value);
  napi_create_uint32(env, tracker->active_processes, &value); napi_set_named_property(env, result, "activeProcesses", value);
  return result;
}

static void tracker_finalize(napi_env env, void *data, void *hint) {
  (void)env; (void)hint;
  resource_tracker *tracker = data;
  if (!tracker) return;
  free(tracker->samples);
  free(tracker);
}

static resource_tracker *unwrap_tracker(napi_env env, napi_value value) {
  void *data = NULL;
  if (napi_get_value_external(env, value, &data) != napi_ok || !data) {
    napi_throw_type_error(env, NULL, "expected a native resource tracker");
    return NULL;
  }
  return data;
}

static napi_value create_tracker(napi_env env, napi_callback_info info) {
  size_t argc = 1; napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) return throw_error(env, "createProcessResourceTracker(pid) requires pid");
  pid_t pid;
  if (!read_pid(env, args[0], &pid)) return NULL;
  resource_tracker *tracker = calloc(1, sizeof(*tracker));
  if (!tracker) return throw_error(env, "resource tracker allocation failed");
  tracker->root_pid = pid;
  initialize_children_baseline(tracker);
  napi_value external;
  if (napi_create_external(env, tracker, tracker_finalize, NULL, &external) != napi_ok) { free(tracker); return throw_error(env, "resource tracker external creation failed"); }
  return external;
}

static napi_value sample_tracker(napi_env env, napi_callback_info info) {
  size_t argc = 1; napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) return throw_error(env, "sampleProcessResourceTracker(tracker) requires tracker");
  resource_tracker *tracker = unwrap_tracker(env, args[0]);
  if (!tracker || !update_tracker(tracker)) return throw_error(env, "process resource query failed");
  return snapshot_value(env, tracker);
}

static napi_value query_tree(napi_env env, napi_callback_info info) {
  size_t argc = 1; napi_value args[1];
  if (napi_get_cb_info(env, info, &argc, args, NULL, NULL) != napi_ok || argc < 1) return throw_error(env, "queryProcessTreeResourceUsage(pid) requires pid");
  pid_t pid;
  if (!read_pid(env, args[0], &pid)) return NULL;
  resource_tracker tracker;
  memset(&tracker, 0, sizeof(tracker)); tracker.root_pid = pid; initialize_children_baseline(&tracker);
  if (!update_tracker(&tracker)) { free(tracker.samples); return throw_error(env, "process resource query failed"); }
  napi_value result = snapshot_value(env, &tracker);
  free(tracker.samples);
  return result;
}

static napi_value init(napi_env env, napi_value exports) {
  napi_property_descriptor properties[] = {
    { "createProcessResourceTracker", NULL, create_tracker, NULL, NULL, NULL, napi_default, NULL },
    { "sampleProcessResourceTracker", NULL, sample_tracker, NULL, NULL, NULL, napi_default, NULL },
    { "queryProcessTreeResourceUsage", NULL, query_tree, NULL, NULL, NULL, napi_default, NULL },
  };
  napi_define_properties(env, exports, sizeof(properties) / sizeof(properties[0]), properties);
  return exports;
}

NAPI_MODULE(NODE_GYP_MODULE_NAME, init)
