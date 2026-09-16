/** The little bit of DOM building the rest of the app does over and over. */

/**
 * Builds an element.
 *
 * Properties are assigned rather than set as attributes, so `onclick`,
 * `checked` and `value` work the way they read; `class`, `for` and anything
 * hyphenated are attributes and are spelled that way.
 */
export function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === "class" || key === "for" || key.includes("-")) {
      node.setAttribute(key, value);
    } else {
      node[key] = value;
    }
  }
  node.append(...children.flat().filter((child) => child != null));
  return node;
}

/** A titled block of controls. */
export function section(title, ...children) {
  return el("section", { class: "section" }, el("h2", {}, title), ...children);
}

/** A labelled row: the name on the left, the control on the right. */
export function row(label, control) {
  return el("label", { class: "row" }, el("span", { class: "row-label" }, label), control);
}
