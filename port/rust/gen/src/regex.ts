// Translates the JS regular expressions used by schema pattern checks into Rust `regex` syntax with the same
// meaning. JS `\d`, `\w` are ASCII and `\s` is JS whitespace, where Rust's are Unicode; JS `.` excludes all line
// terminators, Rust's only `\n`. Constructs Rust's engine lacks (lookaround, backreferences) stop the generator.

const JS_SPACE = "\\t\\n\\x0B\\x0C\\r \\u{A0}\\u{1680}\\u{2000}-\\u{200A}\\u{2028}\\u{2029}\\u{202F}\\u{205F}\\u{3000}\\u{FEFF}"

export function jsRegexToRust(source: string, flags: string): string {
  if (/[^u]/.test(flags)) throw new Error(`unsupported regex flags /${source}/${flags}`)
  let out = ""
  let inClass = false
  for (let i = 0; i < source.length; i++) {
    const c = source[i]
    if (c === "\\") {
      const n = source[++i]
      if (n === undefined) throw new Error(`trailing backslash in /${source}/`)
      switch (n) {
        case "d":
          out += inClass ? "0-9" : "[0-9]"
          break
        case "D":
          if (inClass) throw new Error(`\\D in a class: /${source}/`)
          out += "[^0-9]"
          break
        case "w":
          out += inClass ? "A-Za-z0-9_" : "[A-Za-z0-9_]"
          break
        case "W":
          if (inClass) throw new Error(`\\W in a class: /${source}/`)
          out += "[^A-Za-z0-9_]"
          break
        case "s":
          out += inClass ? JS_SPACE : `[${JS_SPACE}]`
          break
        case "S":
          if (inClass) throw new Error(`\\S in a class: /${source}/`)
          out += `[^${JS_SPACE}]`
          break
        case "/":
          out += "/"
          break
        case "b":
        case "B":
          if (inClass) throw new Error(`\\b in a class: /${source}/`)
          // JS word boundaries are ASCII.
          out += `(?-u:\\${n})`
          break
        default:
          if (/[1-9k]/.test(n)) throw new Error(`backreference in /${source}/`)
          if (n === "u" && source[i + 1] === "{") {
            const end = source.indexOf("}", i)
            out += `\\x{${source.slice(i + 2, end)}}`
            i = end
          } else if (n === "u") {
            out += `\\x{${source.slice(i + 1, i + 5)}}`
            i += 4
          } else out += `\\${n}`
      }
      continue
    }
    if (inClass) {
      if (c === "]") inClass = false
      if (c === "[") out += "\\["
      else if (c === "&" || c === "~" || c === "-") out += c === "-" ? "-" : `\\${c}`
      else out += c
      continue
    }
    if (c === "[" && source.startsWith("[\\s\\S]", i)) {
      // any character
      out += "(?s:.)"
      i += 5
      continue
    }
    if (c === "[") {
      inClass = true
      out += "["
      if (source[i + 1] === "^") {
        out += "^"
        i++
      }
      if (source[i + 1] === "]") throw new Error(`empty class in /${source}/`)
      continue
    }
    if (c === "(" && source[i + 1] === "?" && source[i + 2] !== ":") throw new Error(`lookaround or named group in /${source}/`)
    if (c === ".") {
      out += "[^\\n\\r\\u{2028}\\u{2029}]"
      continue
    }
    out += c
  }
  return out
}
