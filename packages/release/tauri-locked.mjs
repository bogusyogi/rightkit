// The Tauri CLI builds and runs Cargo itself, and an unlocked build can rewrite Cargo.lock inside the snapshot of a commit-only build.
// RustRight refuses a grant-minted `cargo build` or `cargo run` that carries neither `--locked` nor `--frozen` (rightkit#1301), so in a
// snapshot stage with a carrier every `tauri build` and `tauri dev` step gets `--locked` forwarded to Cargo.
//
// tauri-cli 2.12.4 (confirmed from its source by the coordinator; the exact forwarding form is PENDING RustRight's confirmation):
//   tauri build -- --locked              puts --locked on `cargo build` (before --bins), both universal-mac builds included;
//   tauri dev -- --locked                puts it on `cargo run`, before the trailing `--` that run_dev always appends;
//   tauri dev -- --locked -- <appArgs>   when app arguments are passed, --locked stays before the second `--`.
// So `--locked` goes right after the first `--` (added with its own `--` when the step has none). A step that already names `--locked`
// or `--frozen` among the Cargo arguments is left as it is.
import { carriedCargoMapOf } from "./carried-cargo.mjs";

/** `args` of a step that may run the Tauri CLI, with `--locked` forwarded to Cargo in a carried stage; unchanged otherwise. */
export function lockedTauriArgs(args, env = process.env) {
  const list = args.map(String);
  if (!carriedCargoMapOf(env)?.map) return args;
  const at = list.indexOf("tauri");
  if (at < 0 || !["build", "dev"].includes(list[at + 1])) return args;
  const separator = list.indexOf("--", at + 2);
  if (separator < 0) return [...list, "--", "--locked"];
  const own = list.slice(separator + 1);
  const end = own.indexOf("--");
  if ((end < 0 ? own : own.slice(0, end)).some((w) => w === "--locked" || w === "--frozen")) return args;
  return [...list.slice(0, separator + 1), "--locked", ...own];
}
