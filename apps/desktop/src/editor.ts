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

const setBreakpointsEffect = StateEffect.define<BreakpointMark[]>();
const setExecLineEffect = StateEffect.define<number | null>();

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

// CSS-variable backed highlight style so Dark/Light switching is instant
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
    if (line >= 1 && line <= this.view.state.doc.lines) {
      const pos = this.view.state.doc.line(line).from;
      this.view.dispatch({
        effects: EditorView.scrollIntoView(pos, { y: "center" }),
      });
    }
  }

  destroy() {
    if (this.view) {
      this.view.destroy();
      this.view = null;
    }
    this.currentFileName = null;
  }
}
