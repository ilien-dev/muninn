---
description: Turn Muninn on for this project (once per project)
argument-hint: "[--keep-native] [--boot-file]"
---
Run `muninn init $ARGUMENTS` in the project root, then `muninn status`. Show the user the
lines `init` printed (what it touched) and the status line. If `muninn` is not found, the
plugin has not downloaded its binary yet: ask the user to start a new session and run this
command again.

Tell the user in one sentence that Muninn records from this conversation on, and that
`init` does not need to be run again after a plugin update.
