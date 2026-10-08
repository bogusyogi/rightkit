// Publication evidence, rollout staging & adopted pins are separate states.
export function ownershipAvailability(entry, versions) {
  const name = entry.package;
  const ecosystem = name === "RightKitSwift" || name === "rightkit-swift" ? "swift"
    : name?.startsWith("@rightkit/") ? "npm" : "cargo";
  const key = ecosystem === "swift" ? "rightkit-swift" : name;
  const stagedMap = { npm: "stagedNpm", cargo: "stagedCargo", swift: "stagedSwift" }[ecosystem];
  const pinned = versions[ecosystem]?.[key];
  const recorded = versions.registry?.[ecosystem]?.[key];
  const staged = versions[stagedMap]?.[key];
  const versionOf = (value) => typeof value === "string" ? value : value?.version;
  const publishedVersion = versionOf(recorded ?? pinned);
  const rows = [];
  const add = (value, state) => {
    const version = versionOf(value);
    if (version) rows.push({ registry: { npm: "npm", cargo: "crates.io", swift: "SwiftPM" }[ecosystem], package: key, version, state, ...(value?.url ? { url: value.url } : {}) });
  };
  add(recorded ?? pinned, recorded ? "registry-recorded" : "registry-pinned");
  if (versionOf(staged) && versionOf(staged) !== publishedVersion) add(staged, "staged-unverified");
  return rows;
}

export function ownershipDetection(entry) {
  return entry.detection ?? (entry.fingerprints?.length ? "fingerprinted" : "declaration-only");
}
