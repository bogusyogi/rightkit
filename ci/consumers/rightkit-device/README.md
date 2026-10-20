# rightkit-device 0.4.0 registry consumer (PK05)

Standalone consumer, outside the workspace, pinning `rightkit-device =0.4.0` exactly. It exercises
`device_id_hash`, the SHA-256 device-id digest that is the only form of the id sent off the machine.
It checks the FIPS 180-4 vector for "abc", the 64-character lowercase output, determinism, and that
no trimming happens.

State: pure string hashing. No network, no files, no process spawn, no user data. Run only by the
CI consumer lane.

Not covered: `DeviceService`, `SecureService`, the authorization service, and the log and diagnostic
hosts.
