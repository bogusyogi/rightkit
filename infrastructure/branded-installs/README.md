# Branded installer endpoints

`rightapps-branded-installs` maps public hostnames to product-scoped objects in `rightapps-downloads`:

- `legion.orthiclabs.com/install.ps1` -> `legion/install.ps1`
- `membrane.orthiclabs.com/install.ps1` -> `membrane/install.ps1`

Upload each bootstrap with remote R2 upload + readback, then deploy Worker from this directory with pinned pnpm.
