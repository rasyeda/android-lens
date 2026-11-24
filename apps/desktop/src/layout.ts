export interface LayoutSizes {
  leftW: number;
  rightW: number;
  varsPct: number;
  dockH: number;
  layoutCanvasPct: number;
  layoutPropsW: number;
  storageSideW: number;
}

const DEFAULTS: LayoutSizes = {
  leftW: 232,
  rightW: 336,
  varsPct: 56,
  dockH: 224,
  layoutCanvasPct: 42,
  layoutPropsW: 270,
  storageSideW: 230,
};

const STORAGE_KEY = "android-lens:layoutSizes";

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

export class LayoutManager {
  private sizes: LayoutSizes;

  constructor(private readonly root: HTMLElement) {
    this.sizes = this.load();
    this.applyAll();
  }

  private load(): LayoutSizes {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<LayoutSizes>;
        return { ...DEFAULTS, ...parsed };
      }
    } catch {
      // ignore corrupted storage
    }
    return { ...DEFAULTS };
  }

  private save() {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(this.sizes));
  }

  applyAll() {
    this.root.style.setProperty("--left-w", `${this.sizes.leftW}px`);
    this.root.style.setProperty("--right-w", `${this.sizes.rightW}px`);
    this.root.style.setProperty("--vars-pct", `${this.sizes.varsPct}%`);
    this.root.style.setProperty("--dock-h", `${this.sizes.dockH}px`);
    this.root.style.setProperty("--layout-canvas-pct", `${this.sizes.layoutCanvasPct}%`);
    this.root.style.setProperty("--layout-props-w", `${this.sizes.layoutPropsW}px`);
    this.root.style.setProperty("--storage-side-w", `${this.sizes.storageSideW}px`);
  }

  reset() {
    this.sizes = { ...DEFAULTS };
    localStorage.removeItem(STORAGE_KEY);
    this.applyAll();
  }

  bindAll(onDockResize?: () => void) {
    this.bindHorizontal("split-left", {
      get: () => this.sizes.leftW,
      set: (v) => {
        this.sizes.leftW = clamp(Math.round(v), 160, 440);
        this.applyAll();
      },
      reset: () => {
        this.sizes.leftW = DEFAULTS.leftW;
        this.applyAll();
      },
      sign: 1,
    });

    this.bindHorizontal("split-right", {
      get: () => this.sizes.rightW,
      set: (v) => {
        this.sizes.rightW = clamp(Math.round(v), 240, 560);
        this.applyAll();
      },
      reset: () => {
        this.sizes.rightW = DEFAULTS.rightW;
        this.applyAll();
      },
      sign: -1,
    });

    this.bindVerticalPercent("split-inspector", "pane-right", {
      get: () => this.sizes.varsPct,
      set: (pct) => {
        this.sizes.varsPct = clamp(Math.round(pct), 20, 80);
        this.applyAll();
      },
      reset: () => {
        this.sizes.varsPct = DEFAULTS.varsPct;
        this.applyAll();
      },
    });

    this.bindVerticalPx("split-dock", {
      get: () => this.sizes.dockH,
      set: (v) => {
        const maxH = Math.max(240, Math.round(window.innerHeight * 0.72));
        this.sizes.dockH = clamp(Math.round(v), 120, maxH);
        this.applyAll();
        onDockResize?.();
      },
      reset: () => {
        this.sizes.dockH = DEFAULTS.dockH;
        this.applyAll();
        onDockResize?.();
      },
      sign: -1,
    });

    this.bindHorizontalPercent("split-layout-1", "layout-inspector", {
      get: () => this.sizes.layoutCanvasPct,
      set: (pct) => {
        this.sizes.layoutCanvasPct = clamp(Math.round(pct), 22, 62);
        this.applyAll();
      },
      reset: () => {
        this.sizes.layoutCanvasPct = DEFAULTS.layoutCanvasPct;
        this.applyAll();
      },
    });

    this.bindHorizontal("split-layout-2", {
      get: () => this.sizes.layoutPropsW,
      set: (v) => {
        this.sizes.layoutPropsW = clamp(Math.round(v), 200, 460);
        this.applyAll();
      },
      reset: () => {
        this.sizes.layoutPropsW = DEFAULTS.layoutPropsW;
        this.applyAll();
      },
      sign: -1,
    });

    this.bindHorizontal("split-storage", {
      get: () => this.sizes.storageSideW,
      set: (v) => {
        this.sizes.storageSideW = clamp(Math.round(v), 150, 380);
        this.applyAll();
      },
      reset: () => {
        this.sizes.storageSideW = DEFAULTS.storageSideW;
        this.applyAll();
      },
      sign: 1,
    });
  }

  private bindHorizontal(
    id: string,
    opts: { get: () => number; set: (v: number) => void; reset: () => void; sign: 1 | -1 },
  ) {
    const el = document.getElementById(id);
    if (!el) return;

    el.ondblclick = () => {
      opts.reset();
      this.save();
    };

    el.onpointerdown = (e) => {
      e.preventDefault();
      el.setPointerCapture(e.pointerId);
      el.classList.add("dragging");
      document.body.classList.add("resizing-col");
      const startX = e.clientX;
      const startVal = opts.get();

      const onMove = (mv: PointerEvent) => {
        opts.set(startVal + (mv.clientX - startX) * opts.sign);
      };
      const onUp = () => {
        el.classList.remove("dragging");
        document.body.classList.remove("resizing-col");
        el.removeEventListener("pointermove", onMove);
        el.removeEventListener("pointerup", onUp);
        el.removeEventListener("pointercancel", onUp);
        this.save();
      };
      el.addEventListener("pointermove", onMove);
      el.addEventListener("pointerup", onUp);
      el.addEventListener("pointercancel", onUp);
    };
  }

  private bindVerticalPx(
    id: string,
    opts: { get: () => number; set: (v: number) => void; reset: () => void; sign: 1 | -1 },
  ) {
    const el = document.getElementById(id);
    if (!el) return;

    el.ondblclick = () => {
      opts.reset();
      this.save();
    };

    el.onpointerdown = (e) => {
      e.preventDefault();
      el.setPointerCapture(e.pointerId);
      el.classList.add("dragging");
      document.body.classList.add("resizing-row");
      const startY = e.clientY;
      const startVal = opts.get();

      const onMove = (mv: PointerEvent) => {
        opts.set(startVal + (mv.clientY - startY) * opts.sign);
      };
      const onUp = () => {
        el.classList.remove("dragging");
        document.body.classList.remove("resizing-row");
        el.removeEventListener("pointermove", onMove);
        el.removeEventListener("pointerup", onUp);
        el.removeEventListener("pointercancel", onUp);
        this.save();
      };
      el.addEventListener("pointermove", onMove);
      el.addEventListener("pointerup", onUp);
      el.addEventListener("pointercancel", onUp);
    };
  }

  private bindVerticalPercent(
    id: string,
    parentId: string,
    opts: { get: () => number; set: (pct: number) => void; reset: () => void },
  ) {
    const el = document.getElementById(id);
    const parent = document.getElementById(parentId);
    if (!el || !parent) return;

    el.ondblclick = () => {
      opts.reset();
      this.save();
    };

    el.onpointerdown = (e) => {
      e.preventDefault();
      el.setPointerCapture(e.pointerId);
      el.classList.add("dragging");
      document.body.classList.add("resizing-row");
      const startY = e.clientY;
      const startPct = opts.get();
      const totalH = Math.max(100, parent.getBoundingClientRect().height);

      const onMove = (mv: PointerEvent) => {
        const deltaPct = ((mv.clientY - startY) / totalH) * 100;
        opts.set(startPct + deltaPct);
      };
      const onUp = () => {
        el.classList.remove("dragging");
        document.body.classList.remove("resizing-row");
        el.removeEventListener("pointermove", onMove);
        el.removeEventListener("pointerup", onUp);
        el.removeEventListener("pointercancel", onUp);
        this.save();
      };
      el.addEventListener("pointermove", onMove);
      el.addEventListener("pointerup", onUp);
      el.addEventListener("pointercancel", onUp);
    };
  }

  private bindHorizontalPercent(
    id: string,
    parentId: string,
    opts: { get: () => number; set: (pct: number) => void; reset: () => void },
  ) {
    const el = document.getElementById(id);
    const parent = document.getElementById(parentId);
    if (!el || !parent) return;

    el.ondblclick = () => {
      opts.reset();
      this.save();
    };

    el.onpointerdown = (e) => {
      e.preventDefault();
      el.setPointerCapture(e.pointerId);
      el.classList.add("dragging");
      document.body.classList.add("resizing-col");
      const startX = e.clientX;
      const startPct = opts.get();
      const totalW = Math.max(200, parent.getBoundingClientRect().width);

      const onMove = (mv: PointerEvent) => {
        const deltaPct = ((mv.clientX - startX) / totalW) * 100;
        opts.set(startPct + deltaPct);
      };
      const onUp = () => {
        el.classList.remove("dragging");
        document.body.classList.remove("resizing-col");
        el.removeEventListener("pointermove", onMove);
        el.removeEventListener("pointerup", onUp);
        el.removeEventListener("pointercancel", onUp);
        this.save();
      };
      el.addEventListener("pointermove", onMove);
      el.addEventListener("pointerup", onUp);
      el.addEventListener("pointercancel", onUp);
    };
  }
}
