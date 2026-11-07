import { scenario } from "../lib/scenario"
import { say } from "../lib/openrouter"

scenario("text reply to a prompt", { turns: [say("Hello from the model.")] }, async (ctx) => {
  const id = await ctx.session({ title: "basic" })
  await ctx.prompt(id, "Say hello.")
  await ctx.http("GET", `/api/session/${id}`)
  await ctx.http("GET", `/api/session/${id}/message`)
})

scenario("provider and model listing", {}, async (ctx) => {
  await ctx.http("GET", "/api/provider")
  await ctx.http("GET", "/api/model")
})
