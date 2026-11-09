// `--wizard` (effect/cli's wizard mode): the command is built interactively in a terminal, prompt by prompt (Select,
// Confirm, Toggle, text and integer prompts), shown, and run on request.
import { cliScenario } from "../lib/cli"

cliScenario("wizard on the root command, cancelled", {}, async (ctx) => {
  await ctx.terminal(["--wizard"], [{ waitFor: "Command ›", type: "\x03" }], { styles: true })
  // Ctrl+D quits too, and so does the end of input when stdin is not a terminal.
  await ctx.terminal(["--wizard"], [{ waitFor: "Command ›", type: "\x1b[B\x1b[B" }, { waitFor: "❯ acp", type: "\x04" }])
  await ctx.cli(["--wizard"])
  await ctx.cli(["session", "list", "--wizard"])
})

cliScenario("wizard picks a command from the root and runs it", {}, async (ctx) => {
  await ctx.terminal(["--wizard"], [
    { waitFor: "Command ›", type: "\x1b[B\x1b[B\x1b[B\x1b[B" },
    { waitFor: "❯ debug", type: "\r" },
    // Up from the first choice wraps around to the last.
    { waitFor: "❯ agents", type: "\x1b[A" },
    { waitFor: "❯ paths", type: "\r" },
    { waitFor: "Set Name? ›", type: "y" },
    { waitFor: "Name ›", type: "j" },
    { waitFor: "❯ home", type: "\r" },
    { waitFor: "Run this command?", type: "\r" },
  ])
})

cliScenario("wizard with a limit and a format", {}, async (ctx) => {
  await ctx.terminal(
    ["session", "list", "--wizard"],
    [
      { waitFor: "Set Max count (--max-count)? ›", type: "y" },
      // Letters are refused, an empty answer is an error, then a number.
      { waitFor: "Max count (--max-count) ›", type: "abc\r" },
      { waitFor: "Must provide an integer value", type: "3" },
      { waitFor: "Max count (--max-count) › 3", type: "\r" },
      { waitFor: "Set Output format (--format)? ›", type: "y" },
      { waitFor: "Output format (--format) ›", type: "\x1b[B" },
      { waitFor: "❯ json", type: "\r" },
      { waitFor: "Run this command?", type: "\r" },
    ],
    { styles: true },
  )
})

cliScenario("wizard command not run", {}, async (ctx) => {
  await ctx.terminal(["service", "get", "--wizard"], [
    { waitFor: "Set Key? ›", type: "y" },
    { waitFor: "Key ›", type: "port\r" },
    { waitFor: "Set Name? ›", type: "\r" },
    { waitFor: "Run this command?", type: "\x1b[C" },
    { waitFor: "Run this command?", type: "\r" },
  ])
  await ctx.terminal(["service", "get", "--wizard"], [
    { waitFor: "Set Key? ›", type: "y" },
    { waitFor: "Key ›", type: "port\r" },
    { waitFor: "Set Name? ›", type: "n" },
    { waitFor: "Run this command?", type: "\r" },
  ])
})

cliScenario("wizard after flags already given", {}, async (ctx) => {
  await ctx.terminal(["session", "list", "-n", "2", "--wizard"], [
    { waitFor: "Set Max count (--max-count)? ›", type: "\r" },
    { waitFor: "Set Output format (--format)? ›", type: "\r" },
    { waitFor: "Run this command?", type: "\r" },
  ])
  // The wizard asks again for what was given; answering it twice makes the command invalid.
  await ctx.terminal(["session", "list", "--format", "json", "--wizard"], [
    { waitFor: "Set Max count (--max-count)? ›", type: "n" },
    { waitFor: "Set Output format (--format)? ›", type: "y" },
    { waitFor: "Output format (--format) ›", type: "\r" },
    { waitFor: "Run this command?", type: "\r" },
  ])
})

cliScenario("wizard counts, repeated flags and quoting", {}, async (ctx) => {
  await ctx.terminal(["api", "--wizard"], [
    { waitFor: "Set Request body (--data)? ›", type: "\r" },
    { waitFor: "Header (--header) count › 0", type: "\x7f\r" },
    { waitFor: "Must provide an integer value", type: "k" },
    { waitFor: "Header (--header) count › 1", type: "\r" },
    { waitFor: "Header (--header) ›", type: "x-test: it's\r" },
    { waitFor: "Set OpenAPI path or query parameter (--param)? ›", type: "y" },
    { waitFor: "OpenAPI path or query parameter (--param) count › 1", type: "\r" },
    { waitFor: "OpenAPI path or query parameter (--param) ›", type: "sessionID=-odd\r" },
    { waitFor: "Operation | method path count › 1", type: "\x7f3\r" },
    { waitFor: "3 must be less than or equal to 2", type: "\x7f2\r" },
    { waitFor: "Operation | method path ›", type: "GET\r" },
    { waitFor: "… GET\n? Operation | method path ›", type: "/api/info\r" },
    { waitFor: "Run this command?", type: "0" },
    { waitFor: "Run this command?", type: "\r" },
  ])
  await ctx.terminal(["session", "import", "--wizard"], [
    { waitFor: "Set Directory (--directory)? ›", type: "y" },
    { waitFor: "Directory (--directory) ›", type: "-odd dir\r" },
    { waitFor: "File ›", type: "it's.json\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x7f" },
    { waitFor: "File › its.json", type: "\r" },
    { waitFor: "Run this command?", type: "\x1b[B" },
    { waitFor: "Run this command?", type: "\r" },
  ])
})

// An answer the command's own parsing rejects (`stats --days -1`) ends the wizard with the error's `Cause.pretty`,
// stack frames included, which a terminal screen records: not covered.
cliScenario("wizard boolean answers and cancelling mid-way", {}, async (ctx) => {
  await ctx.terminal(["session", "export", "--wizard"], [
    { waitFor: "Set Sanitize (--sanitize)? ›", type: "y" },
    { waitFor: "Sanitize (--sanitize) ›", type: "t" },
    { waitFor: "Set Session? ›", type: "\r" },
    { waitFor: "Run this command?", type: "\t" },
    { waitFor: "Run this command?", type: "\r" },
  ])
  // Cancelled in a text prompt, in an integer prompt that has a value, and with Escape in a choice.
  await ctx.terminal(["service", "get", "--wizard"], [
    { waitFor: "Set Key? ›", type: "y" },
    { waitFor: "Key ›", type: "po\x03" },
  ])
  await ctx.terminal(
    ["stats", "--wizard"],
    [
      { waitFor: "Set Days (--days)? ›", type: "y" },
      { waitFor: "Days (--days) ›", type: "7\x04" },
    ],
    { styles: true },
  )
  await ctx.terminal(["debug", "paths", "--wizard"], [
    { waitFor: "Set Name? ›", type: "y" },
    { waitFor: "Name ›", type: "\x1b" },
  ])
})
