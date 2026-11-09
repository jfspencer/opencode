// An ACP client (`opencode acp`: newline-delimited JSON-RPC 2.0 over stdio) for CLI scenarios. It writes the
// client's requests, notifications and answers to the agent's stdin, and reads the agent's stdout in order:
// requests the agent sends the client (`session/request_permission`, `elicitation/create`) are answered by the
// scenario's handlers as they arrive, so a prompt can wait on its response while the agent asks. Everything is
// recorded by `ctx.start` when the agent exits (stdin as written, stdout as `ndjson`).
import type { CliProcess } from "./cli"

export type Message = { readonly jsonrpc: "2.0"; readonly id?: number | string | null; readonly method?: string; readonly params?: any; readonly result?: any; readonly error?: any }

/** An answer to a request from the agent: a result, an error, or `undefined` to leave it unanswered. */
export type Answer = { readonly result: unknown } | { readonly error: { readonly code: number; readonly message: string; readonly data?: unknown } } | undefined

export type Handler = (params: any, message: Message) => Answer | Promise<Answer>

export interface AcpClient {
  /** Send a request and wait for its response, answering the agent's requests meanwhile. */
  request(method: string, params: unknown, timeout?: number): Promise<Message>
  /** Send a request without waiting; `response(id)` waits for it. */
  send(method: string, params: unknown): number
  /** Wait for the response to a request sent with `send`. */
  response(id: number, timeout?: number): Promise<Message>
  notify(method: string, params: unknown): void
  /** Answer a request from the agent by hand (one a handler left unanswered). */
  answer(id: number | string, answer: NonNullable<Answer>): void
  /** Wait for a message from the agent (from the ones not yet read); requests are dispatched first. */
  until(predicate: (message: Message) => boolean, timeout?: number): Promise<Message>
  /** Replace the handler for requests of a method. */
  handle(method: string, handler: Handler): void
  /** Every message the agent sent so far, parsed. */
  readonly received: Message[]
}

export function acp(agent: CliProcess, handlers: Record<string, Handler> = {}): AcpClient {
  let next = 0
  let read = 0
  const received: Message[] = []
  const table = { ...handlers }
  const complete = () => agent.stdout().split("\n").slice(0, -1)
  const write = (message: object) => agent.write(JSON.stringify(message) + "\n")

  const dispatch = async (message: Message) => {
    if (message.method === undefined || message.id === undefined || message.id === null) return
    const handler = table[message.method]
    if (!handler) return
    const answer = await handler(message.params, message)
    if (answer) write({ jsonrpc: "2.0", id: message.id, ...answer })
  }

  const until = async (predicate: (message: Message) => boolean, timeout = 60_000) => {
    const deadline = Date.now() + timeout
    for (;;) {
      const lines = complete()
      while (read < lines.length) {
        const message = JSON.parse(lines[read++]) as Message
        received.push(message)
        await dispatch(message)
        if (predicate(message)) return message
      }
      const left = deadline - Date.now()
      if (left <= 0) throw new Error(`timed out waiting for an ACP message; stdout: ${agent.stdout()}; stderr: ${agent.stderr()}`)
      await agent.line(() => complete().length > read, "stdout", left)
    }
  }

  const isResponse = (id: number) => (message: Message) => message.id === id && message.method === undefined

  return {
    received,
    send(method, params) {
      const id = ++next
      write({ jsonrpc: "2.0", id, method, params })
      return id
    },
    response: (id, timeout) => until(isResponse(id), timeout),
    async request(method, params, timeout) {
      return until(isResponse(this.send(method, params)), timeout)
    },
    notify(method, params) {
      write({ jsonrpc: "2.0", method, params })
    },
    answer(id, answer) {
      write({ jsonrpc: "2.0", id, ...answer })
    },
    until,
    handle(method, handler) {
      table[method] = handler
    },
  }
}

export const initialize = (client: AcpClient, clientCapabilities: Record<string, unknown> = {}) =>
  client.request("initialize", { protocolVersion: 1, clientCapabilities, clientInfo: { name: "conformance", version: "1.0.0" } })
