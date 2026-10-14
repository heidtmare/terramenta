/** The little bit of DOM building the rest of the app does over and over. */

/**
 * Builds an element.
 *
 * Properties are assigned rather than set as attributes, so `onclick`,
 * `checked` and `value` work the way they read; `class`, `for`, `role` and
 * anything hyphenated are attributes and are spelled that way.
 */
export function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === "class" || key === "for" || key === "role" || key.includes("-")) {
      node.setAttribute(key, value);
    } else {
      node[key] = value;
    }
  }
  node.append(...children.flat().filter((child) => child != null));
  return node;
}

/**
 * A titled, collapsible block of controls. `heading` is a title or
 * `{title, open}` (`open` defaults to true). A state other than the default
 * is remembered per title in `localStorage`.
 */
export function section(heading, ...children) {
  const { title, open = true } = typeof heading === "string" ? { title: heading } : heading;
  const key = `terramenta.section.${title}`;
  const node = el(
    "details",
    {
      class: "section",
      open: remembered(key) ?? open,
      ontoggle: () => remember(key, node.open === open ? undefined : node.open),
    },
    el("summary", {}, el("h2", {}, title)),
    ...children,
  );
  return node;
}

function remembered(key) {
  try {
    const value = localStorage.getItem(key);
    return value === null ? undefined : value === "1";
  } catch {
    return undefined;
  }
}

/** `open` undefined forgets the key. */
function remember(key, open) {
  try {
    if (open === undefined) localStorage.removeItem(key);
    else localStorage.setItem(key, open ? "1" : "0");
  } catch {
    // Storage blocked: the section still toggles, it just isn't remembered.
  }
}

/** A labelled row: the name on the left, the control on the right. */
export function row(label, control) {
  return el("label", { class: "row" }, el("span", { class: "row-label" }, label), control);
}

/**
 * A strip of tabs and the panes it switches between.
 *
 * Which tab is open is the one piece of state this interface keeps for itself:
 * the globe neither sets it nor is told about it. Hidden panes stay in the
 * tree rather than being rebuilt, so every control in them goes on taking its
 * value from each state snapshot and is already current when its tab is opened.
 *
 * Takes `[{id, label, panes}]` and returns the strip, the panes, and how to
 * open one by id.
 */
export function tabs(groups) {
  const buttons = groups.map((group) =>
    el(
      "button",
      {
        class: "panel-tab",
        type: "button",
        role: "tab",
        id: `tab-${group.id}`,
        "data-tab": group.id,
        "aria-controls": `pane-${group.id}`,
        onclick: () => select(group.id),
        onkeydown: (event) => step(event),
      },
      group.label,
    ),
  );

  const panes = groups.map((group) =>
    el(
      "div",
      {
        class: "panel-pane",
        role: "tabpanel",
        id: `pane-${group.id}`,
        "data-tab": group.id,
        "aria-labelledby": `tab-${group.id}`,
      },
      ...group.panes,
    ),
  );

  function select(id) {
    for (const button of buttons) {
      const open = button.dataset.tab === id;
      button.classList.toggle("selected", open);
      button.setAttribute("aria-selected", String(open));
      // Only the open tab is a tab stop: arrow keys move between them, which is
      // how a tab strip is expected to behave.
      button.tabIndex = open ? 0 : -1;
    }
    for (const pane of panes) pane.hidden = pane.dataset.tab !== id;
  }

  function step(event) {
    const offset = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (!offset) return;
    event.preventDefault();
    const index = buttons.indexOf(event.target);
    const next = buttons[(index + offset + buttons.length) % buttons.length];
    select(next.dataset.tab);
    next.focus();
  }

  const strip = el("div", { class: "panel-tabs", role: "tablist" }, ...buttons);
  select(groups[0].id);
  return { strip, panes, select };
}

/** Saves `blob` as `name` through a temporary link. */
export function download(name, blob) {
  const link = el("a", { href: URL.createObjectURL(blob), download: name });
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 0);
}
