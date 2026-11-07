import { say } from "../lib/openrouter"
import { tuiScenario } from "../lib/tui"

tuiScenario("home screen, then a prompt and its reply", { turns: [say("Hello from the model.")] }, async (t) => {
  await t.frame("home")
  await t.type("Say hello")
  await t.frame("typed")
  await t.enter()
  await t.waitFor("Hello from the model.")
  await t.frame("replied")
})
