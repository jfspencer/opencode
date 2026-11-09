// Revert state stored in the shape it had before FileDiff became canonical (`Session.RevertV1`: files with `path`,
// a `diff` string), read back through `PersistedRevert` (packages/schema/src/session-revert.ts, session/info.ts
// `fromRow`). A database written by a v2 release holds it when the release's migration imported an
// `opencode-next.db` (database/v1-migration.bun.ts `importNextDatabase` copies `revert` as stored). The scenario
// stands in for that release: the server runs on a database file, and the row is rewritten underneath it.
import { Database } from "bun:sqlite"
import { mkdtempSync, rmSync } from "node:fs"
import path from "node:path"
import { scenario } from "../lib/scenario"

const legacy = (messageID: string) => [
  // Files in the old shape, with the old `diff`: upgraded to FileDiff.Info.
  {
    messageID,
    partID: "prt_legacy",
    snapshot: "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
    diff: "diff --git a/a.txt b/a.txt",
    files: [
      { path: "a.txt", status: "modified", additions: 2, deletions: 1, patch: "@@ -1 +1,2 @@" },
      { path: "b.txt", status: "added", additions: 3, deletions: 0, patch: "" },
    ],
  },
  // Only the old `diff`: the current shape already fits, and `diff` is dropped.
  { messageID, diff: "diff --git a/a.txt b/a.txt" },
  // The current shape with its keys in another order.
  {
    files: [{ status: "deleted", patch: "", deletions: 4, additions: 0, file: "c.txt" }],
    snapshot: "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
    messageID,
  },
  // Old files without `diff`.
  { messageID, files: [{ path: "d.txt", status: "deleted", additions: 0, deletions: 5, patch: "-x" }] },
]

scenario("revert state from before FileDiff was canonical", { processOnly: "a database file under the server", timeout: 120_000 }, async (ctx) => {
  const dir = mkdtempSync(path.join(path.dirname(ctx.project), "db-"))
  const file = path.join(dir, "opencode.db")
  try {
    await ctx.restart({ OPENCODE_DB: file })
    const ids = [] as string[]
    for (const title of ["legacy files", "legacy diff", "reordered", "legacy files only"])
      ids.push(await ctx.session({ title }))
    const db = new Database(file)
    db.exec("PRAGMA busy_timeout = 5000")
    const messageID = "msg_00000000000000conformanceR1"
    legacy(messageID).forEach((revert, index) =>
      db.query("UPDATE session_v2 SET revert = ? WHERE id = ?").run(JSON.stringify(revert), ids[index]!),
    )
    db.close()
    for (const id of ids) await ctx.http("GET", `/api/session/${id}`)
    await ctx.http("GET", "/api/session")
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
