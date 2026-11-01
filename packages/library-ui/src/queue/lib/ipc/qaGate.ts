export function qaModeEnabled(buildFlag: string | undefined, search: string): boolean {
  return buildFlag === "1" && new URLSearchParams(search).get("qa") === "1";
}
