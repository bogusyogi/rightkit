// PLACEHOLDER. Fails loudly so an unpackaged template can never "release".
// Replace with the mature packaging scripts: GenRight scripts/{mac-dmg,win-nsis,sign-macos-bundle}.mjs
// (managed Cargo target root, DMG mirrored to repo root, NSIS under target/release/bundle/nsis).
console.error(`package:${process.argv[2]} is a template placeholder - copy the packaging scripts (see README step 8).`);
process.exit(1);
