/**
 * LaTeX 定界符归一：将 \( \) 与 \[ \] 转为 remark-math 的 $ 与 $$。
 * 跳过围栏代码块与行内代码；展示数学内部（$$ 之间）保留 \\[2pt] 等换行语法。
 */
interface MathScanState {
  inInline: boolean;
  inDisplay: boolean;
  fence?: { ch: string; len: number };
}

function normalizeLine(line: string, state: MathScanState): string {
  let out = "";
  let i = 0;
  while (i < line.length) {
    const ch = line[i];
    if (ch === "`") {
      let j = i;
      while (j < line.length && line[j] === "`") j++;
      const run = j - i;
      const close = line.indexOf("`".repeat(run), j);
      if (close >= 0) {
        out += line.slice(i, close + run);
        i = close + run;
      } else {
        out += line.slice(i, j);
        i = j;
      }
      continue;
    }
    if (ch === "$" && line[i + 1] === "$") {
      state.inDisplay = !state.inDisplay;
      out += "$$";
      i += 2;
      continue;
    }
    if (ch === "\\" && (i === 0 || line[i - 1] !== "\\")) {
      const next = line[i + 1];
      if (next === "(" && !state.inInline && !state.inDisplay) {
        state.inInline = true;
        out += "$";
        i += 2;
        continue;
      }
      if (next === ")" && state.inInline) {
        state.inInline = false;
        out += "$";
        i += 2;
        continue;
      }
      if (next === "[" && !state.inDisplay) {
        state.inDisplay = true;
        out += "$$";
        i += 2;
        continue;
      }
      if (next === "]" && state.inDisplay) {
        state.inDisplay = false;
        out += "$$";
        i += 2;
        continue;
      }
    }
    out += ch;
    i++;
  }
  return out;
}

export function normalizeMathDelimiters(text: string): string {
  const state: MathScanState = { inInline: false, inDisplay: false };
  const lines = text.split("\n").map((line) => {
    const open = /^[ \t]{0,3}(`{3,}|~{3,})/.exec(line);
    if (state.fence) {
      if (
        open &&
        open[1][0] === state.fence.ch &&
        open[1].length >= state.fence.len
      ) {
        delete state.fence;
      }
      return line;
    }
    if (open) {
      state.fence = { ch: open[1][0], len: open[1].length };
      return line;
    }
    // 展示公式：\[ 独占行首/行尾时转为独立 $$ flow 行（remark-math 仅 flow 形式按展示模式渲染）
    const trimmed = line.trim();
    if (!state.inDisplay && trimmed.startsWith("\\[")) {
      const rest = trimmed.slice(2).trim();
      if (rest.endsWith("\\]")) {
        return "$$\n" + rest.slice(0, -2).trim() + "\n$$";
      }
      state.inDisplay = true;
      return "$$\n" + rest;
    }
    if (state.inDisplay && trimmed.endsWith("\\]")) {
      state.inDisplay = false;
      const rest = trimmed.slice(0, -2).trim();
      return rest ? rest + "\n$$" : "$$";
    }
    return normalizeLine(line, state);
  });
  return lines.join("\n");
}
