// Conditional file reads with the dates clients actually send: `If-Modified-Since` and `If-Range` go through
// `Date.parse` (packages/server/src/handlers/fs.ts), so every form of JavaScriptCore's lenient date grammar decides
// between a 304 / 206 and the full file. README.md's modification time is 2026-01-01T00:00:00Z (the harness's
// first file); each form is sent for that instant and for one second earlier, so a misread by any amount flips one
// of the two answers.
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

// Location boot publishes its inventory events asynchronously; wait for the last one (plugin.updated) so the recorded
// event stream doesn't depend on how fast the scenario body finished.
const scenario = (name: string, options: ScenarioOptions, body: (ctx: Ctx) => Promise<void>) =>
  base(name, options, async (ctx) => {
    await body(ctx)
    await ctx.until((event) => event?.type === "plugin.updated")
  })

const files = { "README.md": "# Demo\n\nA small project.\n" }

// [the modification time, one second earlier], in one form each. The sandbox's time zone is UTC.
const forms: Array<[string, string]> = [
  ["Thu, 01 Jan 2026 00:00:00 GMT", "Wed, 31 Dec 2025 23:59:59 GMT"],
  ["Thursday, 01-Jan-26 00:00:00 GMT", "Wednesday, 31-Dec-25 23:59:59 GMT"],
  ["Thu Jan  1 00:00:00 2026", "Wed Dec 31 23:59:59 2025"],
  ["thu, 1 jan 2026 00:00:00 gmt", "wed, 31 dec 2025 23:59:59 gmt"],
  ["1 January 2026 00:00 UTC", "31 December 2025 23:59:59 UTC"],
  ["January 1, 2026 00:00:00 UT", "December 31, 2025 23:59:59 UT"],
  ["Jan 1 2026", "Dec 31 2025 23:59:59"],
  ["2026-01-01", "2025-12-31T23:59:59Z"],
  ["2026-01-01T00:00:00", "2025-12-31T23:59:59"],
  ["2026-01-01T01:00:00+01:00", "2026-01-01T00:59:59+01:00"],
  ["2026-01-01 00:00:00Z", "2025-12-31 23:59:59Z"],
  ["+002026-01-01T00:00:00.000Z", "+002025-12-31T23:59:59.000Z"],
  ["Wed, 31 Dec 2025 19:00:00 EST", "Wed, 31 Dec 2025 18:59:59 EST"],
  ["Wed, 31 Dec 2025 20:00:00 EDT", "Wed, 31 Dec 2025 19:59:59 EDT"],
  ["Wed, 31 Dec 2025 18:00:00 CST", "Wed, 31 Dec 2025 17:59:59 CST"],
  ["Wed, 31 Dec 2025 16:00:00 PST", "Wed, 31 Dec 2025 15:59:59 PST"],
  ["Thu, 01 Jan 2026 01:00:00 +0100", "Thu, 01 Jan 2026 00:59:59 +0100"],
  ["Thu, 01 Jan 2026 01:30:00 +01:30", "Thu, 01 Jan 2026 01:29:59 +01:30"],
  ["Thu, 01 Jan 2026 01:00:00 GMT+0100", "Thu, 01 Jan 2026 00:59:59 GMT+0100"],
  ["Wed, 31 Dec 2025 22:00:00 GMT-2", "Wed, 31 Dec 2025 21:59:59 GMT-2"],
  ["Thu Jan 01 2026 00:00:00 GMT+0000 (Coordinated Universal Time)", "Wed Dec 31 2025 23:59:59 GMT+0000 (UTC)"],
  ["2026/01/01 00:00:00", "2025/12/31 23:59:59"],
  ["01/01/2026", "12/31/2025 23:59:59"],
  ["1/1/26", "12/31/25 23:59:59"],
  ["Thu, 01 Jan 26 00:00:00 GMT", "Wed, 31 Dec 25 23:59:59 GMT"],
  ["Thu, 01 Jan 2026 12:00:00 AM GMT", "Wed, 31 Dec 2025 11:59:59 PM GMT"],
  ["Thu, 01 Jan 2026 0:0:0 GMT", "Wed, 31 Dec 2025 23:59:59.999 GMT"],
  ["   Thu,  01   Jan   2026   00:00:00   GMT   ", "  Wed,31 Dec 2025 23:59:59 GMT"],
  ["Thu, 01 Jan 2026 00:00:00 Z", "Wed, 31 Dec 2025 23:59:59 z"],
  ["2026 Jan 1 00:00 GMT", "2025 Dec 31 23:59:59 GMT"],
  ["Jan 2026", "Dec 2025"],
  ["Dec 31 2025 24:00:00 GMT", "Dec 31 2025 23:59:59 GMT"],
  ["Thu, 01 Jan 2026 00:00:00 GMT (comment) trailing", "(lead) Wed, 31 Dec 2025 23:59:59 GMT"],
]

// Forms with no instant at all (NaN): never newer, never equal.
const invalid = [
  "",
  "1767225600000",
  "Thu, 32 Jan 2026 00:00:00 GMT",
  "Thu, 01 Foo 2026 00:00:00 GMT",
  "Thu, 01 Jan 2026 25:00:00 GMT",
  "Thu, 01 Jan 2026 00:60:00 GMT",
  "Thu, 01 Jan 2026 00:00:00 XYZ",
  "2026-13-01",
  "2026-01-01T",
  "2026-1-1T00:00:00Z",
  "tomorrow",
  "Jan",
]

scenario("if-modified-since in every date form", { files }, async (ctx) => {
  const read = (since: string) => ctx.http("GET", "/api/fs/read/README.md", undefined, { "if-modified-since": since })
  for (const [same, earlier] of forms) {
    await read(same)
    await read(earlier)
  }
  for (const value of invalid) await read(value)
})

scenario("if-range in every date form", { files }, async (ctx) => {
  const range = (ifRange: string) =>
    ctx.http("GET", "/api/fs/read/README.md", undefined, { range: "bytes=0-3", "if-range": ifRange })
  for (const [same, earlier] of forms) {
    await range(same)
    await range(earlier)
  }
  for (const value of invalid) await range(value)
})
