// `opencode acp` framing (the ACP SDK's ndJsonStream and JSON-RPC connection): blank lines and padding are
// skipped, CRLF line ends accepted, a line that is not JSON answers a parse error, a value that is not a message
// (a number, a string, null, an object without the JSON-RPC envelope) an invalid request, and an answer to a request
// the agent never sent is logged. Unknown notifications and cancels of finished requests change nothing. Each line
// waits for its answer: requests are answered asynchronously, framing errors at once.
import { cliScenario } from "../lib/cli"
import { acp, initialize, type Message } from "../lib/acp"

cliScenario("acp framing of incoming messages", {}, async (ctx) => {
  const agent = ctx.start(["acp"])
  const client = acp(agent)
  const send = async (line: string, answered: (message: Message) => boolean) => {
    agent.write(line)
    await client.until(answered)
  }
  const invalid = (data: unknown) => (message: Message) =>
    message.error?.code === -32600 && JSON.stringify(message.error?.data) === JSON.stringify(data)
  agent.write("\n   \n\t\n")
  await initialize(client)
  await send('  {"jsonrpc":"2.0","id":"padded","method":"authenticate","params":{"methodId":"opencode-login"}}  \n', (m) => m.id === "padded")
  await send('{"jsonrpc":"2.0","id":"crlf","method":"authenticate","params":{"methodId":"opencode-login"}}\r\n', (m) => m.id === "crlf")
  await send("{not json\n", (m) => m.error?.code === -32700)
  await send("42\n", invalid(42))
  await send('"text"\n', invalid("text"))
  await send("null\n", invalid(null))
  await send('{"id":7,"method":"initialize","params":{}}\n', invalid({ id: 7, method: "initialize", params: {} }))
  agent.write('{"jsonrpc":"2.0","id":99,"result":{}}\n')
  agent.write('{"jsonrpc":"2.0","id":"nobody","error":{"code":-32603,"message":"Internal error"}}\n')
  agent.write('{"jsonrpc":"2.0","method":"custom/notification","params":{}}\n')
  agent.write('{"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":1}}\n')
  await client.request("authenticate", { methodId: "opencode-login" })
  agent.end()
  await agent.exited(30_000)
})

// ACP connections refuse JSON-RPC batches (`allowBatches: false`): a batch closes the connection, and the agent
// exits as at the end of its input, without answering it or anything after it.
cliScenario("acp JSON-RPC batches close the connection", {}, async (ctx) => {
  const agent = ctx.start(["acp"])
  const client = acp(agent)
  await initialize(client)
  const authenticate = (id: string) => ({ jsonrpc: "2.0", id, method: "authenticate", params: { methodId: "opencode-login" } })
  agent.write(JSON.stringify([authenticate("batched")]) + "\n" + JSON.stringify(authenticate("after")) + "\n")
  await agent.exited(30_000)
})
