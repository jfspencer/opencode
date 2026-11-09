// The TS reference's @opentui/core 0.8.1 (the compiled package the reference TUI runs, with libopentui.so).
import { realpathSync } from "node:fs"
import path from "node:path"

export const REF = realpathSync(path.resolve(import.meta.dir, "../../../../.reference"))
const entry = Bun.resolveSync("@opentui/core", path.join(REF, "packages/tui"))
export const core = await import(entry)
export const testing = await import(Bun.resolveSync("@opentui/core/testing", path.join(REF, "packages/tui")))
export const CONFORMANCE = realpathSync(path.resolve(import.meta.dir, "../../../../conformance"))
