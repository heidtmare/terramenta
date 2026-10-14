/**
 * The controls the panel is built out of.
 *
 * Every one of them follows the same rule as the panel around it: a control
 * reports what the user did and never sets its own value from a click. Each
 * returns `{node, set}` — the element to place, and how a state snapshot puts
 * it where it belongs — so that whoever placed it can bind the two without
 * knowing how the control is made.
 */

import { el } from "./dom.js";
import { logScale } from "./format.js";

/** Slider positions are `0..1` at this resolution, and the range is applied on top. */
const SLIDER_STEPS = 1000;

export function button(label, onClick, options = {}) {
  return el("button", { class: "button", type: "button", onclick: onClick, ...options }, label);
}

/** A checkbox that reports its new state. */
export function toggle(label, onChange) {
  const input = el("input", {
    type: "checkbox",
    checked: true,
    onchange: (event) => onChange(event.target.checked),
  });
  const node = el("label", { class: "toggle" }, input, el("span", {}, label));
  return {
    node,
    input,
    set: (value) => {
      input.checked = value;
    },
  };
}

/** A set of mutually exclusive buttons. */
export function choice(options, onChange) {
  const buttons = options.map(([value, label]) =>
    el(
      "button",
      { class: "choice-option", type: "button", value, onclick: () => onChange(value) },
      label,
    ),
  );
  const node = el("div", { class: "choice" }, ...buttons);
  return {
    node,
    set: (value) => {
      for (const button of buttons) {
        button.classList.toggle("selected", button.value === value);
      }
    },
  };
}

/** A slider over a range that spans orders of magnitude, with its value shown. */
export function slider({ min, max, format, onInput }) {
  // Held from the moment the thumb is grabbed until it is let go, whether by
  // pointer or by arrow key. The globe reports the altitude it is smoothing
  // toward, so without this the slider would spring back under the pointer.
  let held = false;

  const output = el("span", { class: "slider-value" }, "—");
  const input = el("input", {
    class: "slider",
    type: "range",
    min: "0",
    max: String(SLIDER_STEPS),
    value: "0",
    onpointerdown: () => (held = true),
    onpointerup: () => (held = false),
    onpointercancel: () => (held = false),
    onkeydown: () => (held = true),
    onkeyup: () => (held = false),
    onblur: () => (held = false),
    oninput: (event) => {
      const value = logScale.toValue(Number(event.target.value) / SLIDER_STEPS, min, max);
      output.textContent = format(value);
      onInput(value);
    },
  });
  const node = el("div", { class: "slider-row" }, input, output);
  return {
    node,
    isHeld: () => held,
    set: (value) => {
      const clamped = Math.min(Math.max(value, min), max);
      input.value = String(Math.round(logScale.toPosition(clamped, min, max) * SLIDER_STEPS));
      output.textContent = format(value);
    },
  };
}

/** A growable list of sub-forms; `make(value?)` → `{fields, value()}`. */
export function rowList(make, addLabel) {
  const entries = [];
  const list = el("div", { class: "layers" });
  const add = (value) => {
    const entry = make(value);
    const node = el(
      "div",
      { class: "layer" },
      ...entry.fields,
      el(
        "div",
        { class: "buttons" },
        button("Remove", () => {
          node.remove();
          entries.splice(entries.indexOf(entry), 1);
        }),
      ),
    );
    entries.push(entry);
    list.append(node);
  };
  return {
    node: el("div", {}, list, el("div", { class: "buttons" }, button(addLabel, () => add()))),
    add,
    clear() {
      entries.length = 0;
      list.replaceChildren();
    },
    values: () => entries.map((entry) => entry.value()),
    entries: () => entries,
  };
}

/**
 * Sets a text-ish input from the globe, unless the user is in the middle of
 * typing into it.
 *
 * The one-way rule needs this escape hatch for anything with a caret in it: a
 * value written back while a field has focus moves the caret to the end, which
 * makes editing a URL or a refresh period impossible.
 */
export function setUnlessFocused(input, value) {
  if (document.activeElement !== input && input.value !== value) {
    input.value = value;
  }
}

