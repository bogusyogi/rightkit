// Enforcing hosts launch only protected binaries with an injection-free environment.
export function protectedChildEnvironment(input = process.env, platform = process.platform) {
  const env = Object.fromEntries(Object.entries(input).filter(([name]) => !/^(?:GIT_|NODE_|LD_|DYLD_|BASH|ENV$|SHELLOPTS$|PSModulePath$|PSExecutionPolicyPreference$|PATH$)/i.test(name)));
  env.PATH = platform === "win32" ? "C:\\Windows\\System32;C:\\Program Files\\Git\\cmd" : "/usr/bin:/bin:/usr/sbin:/sbin";
  if (platform === "win32") env.PSModulePath = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\Modules";
  env.GIT_CONFIG_NOSYSTEM = "1";
  env.GIT_CONFIG_GLOBAL = platform === "win32" ? "NUL" : "/dev/null";
  env.GIT_OPTIONAL_LOCKS = "0";
  env.GIT_TERMINAL_PROMPT = "0";
  env.GIT_NO_REPLACE_OBJECTS = "1";
  return env;
}

export function nativeGitExecutable(platform = process.platform) {
  if (platform === "win32") return "C:\\Program Files\\Git\\cmd\\git.exe";
  return "/usr/bin/git";
}
