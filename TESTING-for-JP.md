# Interactive Erlkönig checks

Use only the explicitly selected native candidate. The older four-executable
bundle remains rollback, not evidence for this implementation. Do not overwrite
an app while it is running. Stop any legacy Erlkönig node before migration;
the app deliberately refuses to share its data-owner lock with it.

These are pending interactive checks, not an already-passed checklist:

1. Launch the candidate. Expect one crown, no Dock window, and a clickable
   status line. A fresh app store reports sync off. No background-node approval
   or helper process should appear.
2. Copy my ID, quit, reopen and confirm the same identity. Quit must remove the
   crown and local MCP listener, not leave a resident sync service. Force Quit
   must remove app-owned native threads/listeners as well.
3. Enable Open at Login only when requested. This registers the main app, not
   an agent. Check logout/login separately; ad-hoc signing can require approval
   after a new build changes the code identity.
4. Confirm the static crown and Reduce Motion. With a disposable configured
   sync fixture, confirm real transfer activity and settling. Current receive
   counters update after reconcile, so continuous long-transfer motion is not
   yet established by this implementation.
5. Client setup is a separate explicit action. Do not click Connect in a real
   profile during validation. A scratch launch must isolate HOME as well as
   CODEX_HOME/CLAUDE_CONFIG_DIR, since menu installers currently use HOME paths.
   Headless scratch probes establish HTTP client support without this action.

Normal Quit cooperatively drains; after 15 seconds a stuck native operation
causes final process termination with ordinary Force Quit durability limits.
Do not describe the library's five-second transport-close timeout as a bound
on all admitted work.

Claude Code supports the HTTP configuration. Claude chat's remote connectors
cannot reach this machine's loopback address. Cowork has native local-plugin
execution, but this specific authenticated HTTP connection remains unverified.
There is no hidden stdio bridge or unauthenticated fallback.
