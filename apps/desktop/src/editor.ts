import { EditorState, RangeSet, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  DecorationSet,
  EditorView,
  GutterMarker,
  gutter,
  lineNumbers,
} from "@codemirror/view";
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from "@codemirror/language";
import { java, kotlin } from "@codemirror/legacy-modes/mode/clike";
import { tags } from "@lezer/highlight";

export interface BreakpointMark {
  line: number;
  status: "active" | "pending";
}

export interface FindState {
  count: number;
  index: number; // 0-based active match, or -1 if none
}

class BreakpointMarker extends GutterMarker {
  constructor(private readonly status: "active" | "pending") {
    super();
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = `bp-dot ${this.status}`;
    el.title =
      this.status === "active"
        ? "Active breakpoint (click to remove)"
        : "Pending — waiting for class to load (click to remove)";
    return el;
  }
}

class ExecLineMarker extends GutterMarker {
  toDOM() {
    const el = document.createElement("span");
    el.className = "exec-arrow";
    el.title = "Paused execution line";
    return el;
  }
}

interface SearchSpec {
  ranges: { from: number; to: number }[];
  activeIndex: number;
}

const setBreakpointsEffect = StateEffect.define<BreakpointMark[]>();
const setExecLineEffect = StateEffect.define<number | null>();
const setSearchEffect = StateEffect.define<SearchSpec>();

const breakpointField = StateField.define<RangeSet<GutterMarker>>({
  create() {
    return RangeSet.empty;
  },
  update(markers, tr) {
    let next = markers.map(tr.changes);
    for (const eff of tr.effects) {
      if (eff.is(setBreakpointsEffect)) {
        const sorted = [...eff.value]
          .filter((bp) => bp.line >= 1 && bp.line <= tr.state.doc.lines)
          .sort((a, b) => a.line - b.line);
        const ranges = sorted.map((bp) => {
          const line = tr.state.doc.line(bp.line);
          return new BreakpointMarker(bp.status).range(line.from);
        });
        next = RangeSet.of(ranges, true);
      }
    }
    return next;
  },
});

const execLineGutterField = StateField.define<RangeSet<GutterMarker>>({
  create() {
    return RangeSet.empty;
  },
  update(markers, tr) {
    let next = markers.map(tr.changes);
    for (const eff of tr.effects) {
      if (eff.is(setExecLineEffect)) {
        if (eff.value !== null && eff.value >= 1 && eff.value <= tr.state.doc.lines) {
          const line = tr.state.doc.line(eff.value);
          next = RangeSet.of([new ExecLineMarker().range(line.from)]);
        } else {
          next = RangeSet.empty;
        }
      }
    }
    return next;
  },
});

const execLineDeco = Decoration.line({ class: "cm-exec-line" });

const execLineField = StateField.define<DecorationSet>({
  create() {
    return Decoration.none;
  },
  update(decos, tr) {
    let next = decos.map(tr.changes);
    for (const eff of tr.effects) {
      if (eff.is(setExecLineEffect)) {
        if (eff.value !== null && eff.value >= 1 && eff.value <= tr.state.doc.lines) {
          const line = tr.state.doc.line(eff.value);
          next = Decoration.set([execLineDeco.range(line.from)]);
        } else {
          next = Decoration.none;
        }
      }
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const matchMark = Decoration.mark({ class: "cm-find-match" });
const activeMatchMark = Decoration.mark({ class: "cm-find-match-active" });

const searchField = StateField.define<DecorationSet>({
  create() {
    return Decoration.none;
  },
  update(decos, tr) {
    let next = decos.map(tr.changes);
    for (const eff of tr.effects) {
      if (eff.is(setSearchEffect)) {
        const { ranges, activeIndex } = eff.value;
        if (ranges.length === 0) {
          next = Decoration.none;
        } else {
          const marks = ranges.map((r, i) =>
            (i === activeIndex ? activeMatchMark : matchMark).range(r.from, r.to),
          );
          next = Decoration.set(marks, true);
        }
      }
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

// CSS-variable backed highlight style so live theme switching is instant
const semanticHighlight = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--al-syn-kw)" },
  { tag: [tags.name, tags.deleted, tags.character, tags.macroName], color: "var(--al-text)" },
  { tag: [tags.function(tags.variableName), tags.labelName], color: "var(--al-syn-fn)" },
  { tag: [tags.color, tags.constant(tags.name), tags.standard(tags.name)], color: "var(--al-syn-num)" },
  { tag: [tags.definition(tags.name), tags.separator], color: "var(--al-text)" },
  { tag: [tags.typeName, tags.className, tags.changed, tags.annotation], color: "var(--al-syn-type)" },
  { tag: [tags.number, tags.atom], color: "var(--al-syn-num)" },
  { tag: [tags.operator, tags.operatorKeyword], color: "var(--al-syn-op)" },
  { tag: [tags.string, tags.meta, tags.regexp], color: "var(--al-syn-str)" },
  { tag: tags.comment, color: "var(--al-syn-comment)", fontStyle: "italic" },
]);

export class SourceViewer {
  private view: EditorView | null = null;
  private currentFileName: string | null = null;
  private lastRanges: { from: number; to: number }[] = [];
  private lastMatchIndex = -1;

  constructor(
    private readonly container: HTMLElement,
    private readonly onToggleBreakpoint: (fileName: string, line: number) => void,
  ) {}

  openDocument(
    fileName: string,
    content: string,
    language: string,
    breakpoints: BreakpointMark[],
    execLine: number | null,
  ) {
    this.currentFileName = fileName;
    this.lastRanges = [];
    this.lastMatchIndex = -1;
    if (this.view) {
      this.view.destroy();
      this.view = null;
    }

    const langExt =
      language === "kotlin"
        ? [StreamLanguage.define(kotlin)]
        : language === "java"
          ? [StreamLanguage.define(java)]
          : [];

    const bpGutter = gutter({
      class: "cm-bp-gutter",
      markers: (v) => v.state.field(breakpointField),
      domEventHandlers: {
        mousedown: (view, line) => {
          if (!this.currentFileName) return false;
          const lineNo = view.state.doc.lineAt(line.from).number;
          this.onToggleBreakpoint(this.currentFileName, lineNo);
          return true;
        },
      },
    });

    const execGutter = gutter({
      class: "cm-exec-gutter",
      markers: (v) => v.state.field(execLineGutterField),
    });

    const state = EditorState.create({
      doc: content,
      extensions: [
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
        breakpointField,
        execLineGutterField,
        execLineField,
        searchField,
        bpGutter,
        execGutter,
        lineNumbers(),
        syntaxHighlighting(semanticHighlight),
        ...langExt,
      ],
    });

    this.view = new EditorView({
      state,
      parent: this.container,
    });

    this.setBreakpoints(breakpoints);
    this.setExecutionLine(execLine, execLine !== null);
  }

  hasDocument(): boolean {
    return this.view !== null;
  }

  getLineCount(): number {
    return this.view ? this.view.state.doc.lines : 0;
  }

  setBreakpoints(breakpoints: BreakpointMark[]) {
    if (!this.view) return;
    this.view.dispatch({
      effects: setBreakpointsEffect.of(breakpoints),
    });
  }

  setExecutionLine(line: number | null, scroll = true) {
    if (!this.view) return;
    const effects: StateEffect<unknown>[] = [setExecLineEffect.of(line)];
    if (scroll && line !== null && line >= 1 && line <= this.view.state.doc.lines) {
      const pos = this.view.state.doc.line(line).from;
      effects.push(EditorView.scrollIntoView(pos, { y: "center" }));
    }
    this.view.dispatch({ effects });
  }

  scrollToLine(line: number) {
    if (!this.view) return;
    const clamped = Math.max(1, Math.min(line, this.view.state.doc.lines));
    const pos = this.view.state.doc.line(clamped).from;
    this.view.dispatch({
      effects: EditorView.scrollIntoView(pos, { y: "center" }),
    });
  }

  find(query: string, caseSensitive: boolean): FindState {
    if (!this.view || !query) {
      this.clearFind();
      return { count: 0, index: -1 };
    }

    const docText = this.view.state.doc.toString();
    const haystack = caseSensitive ? docText : docText.toLowerCase();
    const needle = caseSensitive ? query : query.toLowerCase();

    const ranges: { from: number; to: number }[] = [];
    let from = 0;
    while (ranges.length < 2000) {
      const idx = haystack.indexOf(needle, from);
      if (idx === -1) break;
      ranges.push({ from: idx, to: idx + needle.length });
      from = idx + Math.max(1, needle.length);
    }

    this.lastRanges = ranges;
    this.lastMatchIndex = ranges.length > 0 ? 0 : -1;
    this.dispatchFind(true);
    return { count: ranges.length, index: this.lastMatchIndex };
  }

  stepFind(delta: 1 | -1): FindState {
    if (!this.view || this.lastRanges.length === 0) {
      return { count: 0, index: -1 };
    }
    const n = this.lastRanges.length;
    this.lastMatchIndex = (this.lastMatchIndex + delta + n) % n;
    this.dispatchFind(true);
    return { count: n, index: this.lastMatchIndex };
  }

  clearFind() {
    this.lastRanges = [];
    this.lastMatchIndex = -1;
    this.dispatchFind(false);
  }

  private dispatchFind(scroll: boolean) {
    if (!this.view) return;
    const effects: StateEffect<unknown>[] = [
      setSearchEffect.of({ ranges: this.lastRanges, activeIndex: this.lastMatchIndex }),
    ];
    if (scroll && this.lastMatchIndex >= 0 && this.lastMatchIndex < this.lastRanges.length) {
      effects.push(
        EditorView.scrollIntoView(this.lastRanges[this.lastMatchIndex].from, { y: "center" }),
      );
    }
    this.view.dispatch({ effects });
  }

  destroy() {
    if (this.view) {
      this.view.destroy();
      this.view = null;
    }
    this.currentFileName = null;
    this.lastRanges = [];
    this.lastMatchIndex = -1;
  }
}
