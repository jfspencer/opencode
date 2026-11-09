// Cases of the store milestone (createData): hand-picked scripts of the races data.ts is written for, and
// generated ones (harness/cases/data-generator.ts), in several configurations.
const SSE_JSON = [["content-type", "application/json"]]
const ok = (request: unknown, body: unknown) => ({ op: "respond", request, status: 200, headers: SSE_JSON, chunks: [JSON.stringify(body)] })
const info = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  projectID: "prj_main",
  cost: 0.5,
  tokens: { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } },
  time: { created: 1, updated: 2 },
  location: { directory: "/project" },
  ...extra,
})
const event = (id: number, type: string, data: Record<string, unknown>, extra: Record<string, unknown> = {}) => ({
  id: `evt_${String(id).padStart(4, "0")}`,
  created: 1000 + id,
  type,
  data,
  ...extra,
})

function fixed() {
  const cases: { name: string; steps: unknown[]; [key: string]: unknown }[] = [
    {
      name: "connect hydrates active sessions and catalogs",
      steps: [
        { op: "events", events: [event(1, "server.connected", {})] },
        ok({ match: "GET /api/session/active" }, { data: { ses_a: { type: "busy" } } }),
        ok({ match: "GET /api/location" }, { directory: "/project", project: { id: "prj_main" } }),
        ok({ match: "GET /api/vcs" }, { location: { directory: "/project" }, data: { branch: "main" } }),
        ok({ match: "GET /api/project" }, [{ id: "prj_main", time: { created: 1, active: 2 } }]),
        ok({ match: "GET /api/session/ses_a" }, { data: info("ses_a") }),
      ],
    },
    {
      name: "events overtake the active snapshot",
      steps: [
        { op: "events", events: [event(1, "server.connected", {})] },
        { op: "events", events: [event(2, "session.execution.succeeded", { sessionID: "ses_a" }), event(3, "session.execution.started", { sessionID: "ses_b" })] },
        ok({ match: "GET /api/session/active" }, { data: { ses_a: { type: "busy" }, ses_c: { type: "busy" } } }),
      ],
    },
    {
      name: "a streamed turn",
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [], cursor: {} }),
        {
          op: "events",
          events: [
            event(1, "session.step.started", { sessionID: "ses_a", assistantMessageID: "msg_a1", agent: "build", model: { providerID: "p", id: "m" }, started: 5 }),
            event(2, "session.text.started", { sessionID: "ses_a", assistantMessageID: "msg_a1" }),
            event(3, "session.text.delta", { sessionID: "ses_a", assistantMessageID: "msg_a1", delta: "Hel" }),
            event(4, "session.text.delta", { sessionID: "ses_a", assistantMessageID: "msg_a1", delta: "lo" }),
            event(5, "session.tool.input.started", { sessionID: "ses_a", assistantMessageID: "msg_a1", id: "call_1", name: "bash" }),
            event(6, "session.tool.called", { sessionID: "ses_a", assistantMessageID: "msg_a1", id: "call_1", input: { cmd: "ls" } }),
          ],
        },
        { op: "events", events: [event(7, "session.tool.success", { sessionID: "ses_a", assistantMessageID: "msg_a1", id: "call_1", metadata: {}, content: [{ type: "text", text: "a" }] })] },
        { op: "events", events: [event(8, "session.step.ended", { sessionID: "ses_a", assistantMessageID: "msg_a1", finish: "stop" }), event(9, "session.execution.succeeded", { sessionID: "ses_a" })] },
      ],
    },
    {
      name: "a prompt admitted optimistically, echoed, then delivered",
      steps: [
        { op: "call", path: "session.prompt", args: [{ sessionID: "ses_a", id: "msg_p1", text: "hello", files: [{ uri: "data:text/plain;base64,aGk=", name: "a.txt" }] }] },
        { op: "events", events: [event(1, "session.inbox.enqueued", { sessionID: "ses_a", inboxID: "msg_p1", item: { type: "user", delivery: "steer", payload: { text: "hello" } } })] },
        ok({ match: "POST /api/session/ses_a/prompt" }, { data: { id: "msg_p1" } }),
        { op: "events", events: [event(2, "session.inbox.delivered", { sessionID: "ses_a", inboxID: "msg_p1" })] },
      ],
    },
    {
      name: "a rejected prompt rolls back",
      steps: [
        { op: "call", path: "session.prompt", args: [{ sessionID: "ses_a", id: "msg_p1", text: "hello" }] },
        { op: "respond", request: { match: "POST /api/session/ses_a/prompt" }, status: 409, headers: SSE_JSON, chunks: [JSON.stringify({ _tag: "ConflictError", message: "busy" })] },
      ],
    },
    {
      name: "a create then a prompt waits for it",
      steps: [
        { op: "call", path: "session.create", args: [{ id: "ses_new1", agent: "build" }] },
        { op: "call", path: "session.prompt", args: [{ sessionID: "ses_new1", id: "msg_p1", text: "first" }] },
        ok({ match: "POST /api/session" }, { data: info("ses_new1", { title: "Created" }) }),
        ok({ match: "POST /api/session/ses_new1/prompt" }, { data: { id: "msg_p1" } }),
      ],
    },
    {
      name: "a failed create removes its session and fails the prompt",
      steps: [
        { op: "call", path: "session.create", args: [{ id: "ses_new1" }] },
        { op: "call", path: "session.prompt", args: [{ sessionID: "ses_new1", id: "msg_p1", text: "first" }] },
        { op: "respond", request: { match: "POST /api/session" }, status: 503, headers: SSE_JSON, chunks: [JSON.stringify({ _tag: "ServiceUnavailableError", message: "no" })] },
      ],
    },
    {
      name: "compaction coalesces onto the server's id",
      steps: [
        { op: "call", path: "session.compact", args: [{ sessionID: "ses_a" }] },
        { op: "events", events: [event(1, "session.inbox.enqueued", { sessionID: "ses_a", inboxID: "msg_server", item: { type: "compaction", delivery: "steer", payload: {} } })] },
        ok({ match: "POST /api/session/ses_a/compact" }, { data: { id: "msg_server", sessionID: "ses_a", type: "compaction", delivery: "steer", time: { created: 1 }, payload: {} } }),
        { op: "events", events: [event(2, "session.compaction.started", { sessionID: "ses_a", inputID: "msg_server", reason: "manual" }), event(3, "session.compaction.delta", { sessionID: "ses_a", text: "sum" })] },
        { op: "events", events: [event(4, "session.compaction.ended", { sessionID: "ses_a", reason: "manual", text: "summary" })] },
      ],
    },
    {
      name: "pending sync reconciles events that overtook it",
      steps: [
        { op: "call", path: "session.pending.sync", args: ["ses_a"] },
        { op: "events", events: [event(1, "session.inbox.enqueued", { sessionID: "ses_a", inboxID: "msg_q1", item: { type: "user", delivery: "queue", payload: { text: "q" } } })] },
        { op: "events", events: [event(2, "session.inbox.delivery.changed", { sessionID: "ses_a", inboxID: "msg_q0", delivery: "steer" }), event(3, "session.inbox.cancelled", { sessionID: "ses_a", inboxID: "msg_q2" })] },
        ok({ match: "GET /api/session/ses_a/inbox" }, { data: [
          { id: "msg_q0", sessionID: "ses_a", type: "user", delivery: "queue", time: { created: 1 }, payload: { text: "zero" } },
          { id: "msg_q2", sessionID: "ses_a", type: "user", delivery: "steer", time: { created: 2 }, payload: { text: "two" } },
        ] }),
        { op: "call", path: "session.pending.status", args: ["ses_a", "msg_q0"] },
        { op: "call", path: "session.pending.status", args: ["ses_a", "msg_q1"] },
      ],
    },
    {
      name: "a deleted session is not resurrected by an older read",
      steps: [
        { op: "call", path: "session.sync", args: ["ses_a"] },
        { op: "events", events: [event(1, "session.deleted", { sessionID: "ses_a" })] },
        ok({ match: "GET /api/session/ses_a" }, { data: info("ses_a") }),
        { op: "call", path: "session.missing", args: ["ses_a"] },
      ],
    },
    {
      name: "invalidated loads revalidate serially",
      steps: [
        { op: "call", path: "session.sync", args: ["ses_a"] },
        { op: "call", path: "session.invalidate", args: ["ses_a"] },
        { op: "call", path: "session.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a" }, { data: info("ses_a", { title: "first" }) }),
        ok({ match: "GET /api/session/ses_a" }, { data: info("ses_a", { title: "second" }) }),
      ],
    },
    {
      name: "a disconnect makes cached reads stale",
      steps: [
        { op: "call", path: "project.sync", args: [] },
        ok({ match: "GET /api/project" }, []),
        { op: "status", value: "reconnecting" },
        { op: "call", path: "project.sync", args: [] },
        { op: "status", value: "connected" },
        ok({ match: "GET /api/project" }, [{ id: "prj_main", time: { created: 1, active: 2 } }]),
      ],
    },
    {
      name: "bursts of mcp status refetch once",
      steps: [
        { op: "call", path: "location.mcp.server.sync", args: [] },
        ok({ match: "GET /api/mcp" }, { location: { directory: "/project" }, data: [] }),
        { op: "events", events: [event(1, "mcp.status.changed", {}, { location: { directory: "/project" } })] },
        { op: "advance", ms: 100 },
        { op: "events", events: [event(2, "mcp.status.changed", {}, { location: { directory: "/project" } })] },
        { op: "advance", ms: 149 },
        { op: "advance", ms: 1 },
        ok({ match: "GET /api/mcp" }, { location: { directory: "/project" }, data: [{ id: "srv" }] }),
      ],
    },
    {
      name: "loading older pages",
      initialMessageLimit: 2,
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_3", type: "user", text: "c", time: { created: 3 } }, { id: "msg_2", type: "user", text: "b", time: { created: 2 } }], cursor: { next: "older" } }),
        { op: "call", path: "session.message.loadMore", args: ["ses_a"] },
        { op: "call", path: "session.message.loadMore", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_1", type: "user", text: "a", time: { created: 1 } }], cursor: {} }),
        { op: "call", path: "session.message.more", args: ["ses_a"] },
      ],
    },
    // The TUI's jump to the first message pages with a signal it aborts when the view moves on.
    ...[
      { name: "a history jump aborted while its page is in flight", reason: { name: "AbortError" } },
      { name: "a history jump aborted with a reason of its own", reason: { name: "Error", message: "moved on" } },
    ].map(({ name, reason }) => ({
      name,
      initialMessageLimit: 2,
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_3", type: "user", text: "c", time: { created: 3 } }], cursor: { next: "older" } }),
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { all: true, signal: { $signal: 1 } }] },
        { op: "call", path: "session.message.loading", args: ["ses_a"] },
        { op: "abort", signal: 1, reason },
        { op: "call", path: "session.message.loading", args: ["ses_a"] },
        { op: "call", path: "session.message.more", args: ["ses_a"] },
        // A later jump pages again from the same cursor.
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { all: true, signal: { $signal: 2 } }] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_2", type: "user", text: "b", time: { created: 2 } }], cursor: { next: "oldest" } }),
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_1", type: "user", text: "a", time: { created: 1 } }], cursor: {} }),
      ],
    })),
    {
      name: "a history jump aborted while it waits for the first page",
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { all: true, signal: { $signal: 1 } }] },
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { signal: { $signal: 2 } }] },
        { op: "abort", signal: 1, reason: { name: "AbortError" } },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_3", type: "user", text: "c", time: { created: 3 } }], cursor: { next: "older" } }),
        ok({ match: "GET /api/session/ses_a/message" }, { data: [{ id: "msg_2", type: "user", text: "b", time: { created: 2 } }], cursor: {} }),
        { op: "abort", signal: 2, reason: { name: "AbortError" } },
      ],
    },
    {
      name: "a history jump whose signal is already aborted",
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [], cursor: { next: "older" } }),
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { all: true, signal: { $signal: 1, aborted: { name: "AbortError" } } }] },
        { op: "call", path: "session.message.loading", args: ["ses_a"] },
      ],
    },
    {
      name: "a failed page while a jump is aborted",
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, { data: [], cursor: { next: "older" } }),
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { signal: { $signal: 1 } }] },
        { op: "call", path: "session.message.loadMore", args: ["ses_a", { all: true, signal: { $signal: 2 } }] },
        { op: "abort", signal: 2, reason: { name: "AbortError" } },
        { op: "respond", request: { match: "GET /api/session/ses_a/message" }, status: 500, headers: SSE_JSON, chunks: [JSON.stringify({ _tag: "UnknownError", message: "boom" })] },
        { op: "call", path: "session.message.loading", args: ["ses_a"] },
      ],
    },
    {
      name: "tool file data omitted",
      omitToolFileData: true,
      steps: [
        { op: "call", path: "session.message.sync", args: ["ses_a"] },
        ok({ match: "GET /api/session/ses_a/message" }, {
          data: [{ id: "msg_a", type: "assistant", agent: "b", model: {}, time: { created: 1 }, content: [{ type: "tool", id: "c", name: "read", time: {}, state: { status: "completed", input: {}, metadata: {}, content: [{ type: "file", uri: "data:image/png;base64,AAAA", mime: "image/png" }] } }] }],
          cursor: {},
        }),
      ],
    },
  ]
  return cases.map((spec) => ({ kind: "data", ...spec, name: `data ${spec.name}` }))
}

export function dataCases(seed: number, count: number) {
  const generated = Array.from({ length: count }, (_, index) => ({
    kind: "data",
    name: `data #${index}`,
    generate: { seed: seed * 100_003 + index, steps: 20 + ((index * 7) % 60) },
    ...(index % 5 === 1 ? { initialMessageLimit: 3 } : {}),
    ...(index % 7 === 2 ? { omitToolFileData: true } : {}),
    ...(index % 3 === 0 ? { onError: true } : {}),
    ...(index % 11 === 4 ? { status: "connecting" } : {}),
  }))
  return { fixed: fixed(), generated }
}
