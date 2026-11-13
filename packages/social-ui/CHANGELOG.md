# Changelog

## 0.1.1

- Fixed: `@rightkit/social-ui/data` and `@rightkit/social-ui/data/source` are importable. The README sends hosts to these two modules by subpath (`selectDataSource`, `DataSource`, `DateRange` are left out of the root entry), but the `exports` map did not list them.

## 0.1.0

- First release. Social media management UI: accounts, drafts, approvals and analytics screens with their data layer.
