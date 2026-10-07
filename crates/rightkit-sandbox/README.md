# rightkit-sandbox

Shared sandbox, PTY, process-tree, and resource authority for Right Suite applications. It provides sandbox policies, PTY lease fencing, bounded process-tree handling, and reserve-before-spawn resource control.

Confinement backends: macOS Seatbelt, Linux Landlock/seccomp, and Windows AppContainer (ported from CodeRight). On Windows the policy's paths are ACLed to a per-grant-set container SID only while a child that needs them is alive, network is the `internetClient` capability (loopback stays blocked even then: an AppContainer loopback exemption needs elevation), and every child is created suspended inside a kill-on-close Job Object. Sandboxed PTYs remain macOS-only.

Publish through the RightKit release workflow.
