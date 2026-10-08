# Erlkönig package sources

This directory is source material, not an installable mixed-provider package.
The app generates one OpenAI package and one Claude package with the same
`skills/faculties/SKILL.md`. Each receives only its provider manifest and an
`.mcp.json` command with the absolute current app executable, data directory,
and local endpoint. Paths are separate argument values, not a shell command.

Claude Code and the manually uploaded Cowork ZIP use the same Claude package.
There is no standalone Desktop Chat extension. Generated packages contain no
credential, private key, pile, top-level `bin/` folder, or bundled executable.
The client invokes the app binary's `mcp-stdio` mode; that transport only
forwards to an already-running app. Quit closes the owning backend, and the
transport must never relaunch it or acquire a pile itself.
