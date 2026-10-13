/**
 * Project section: current project, bundled projects, open (file/URL), save.
 * Every open reloads the page onto a fresh globe — see `project.js`.
 */

import { el, row, section } from "./dom.js";
import {
  PROJECT_EXTENSION,
  captureProject,
  downloadProject,
  loadManifest,
  openEmptyProject,
  openProjectFile,
  openProjectUrl,
} from "./project.js";
import { button } from "./widgets.js";

/** `bind` as in `panel.js`; `project` is the loaded project, `source` its path or file name. */
export function mountProjectSection(bind, project, source) {
  let lastState = null;

  const error = el("p", { class: "detail error", hidden: true });
  const report = (caught) => {
    error.textContent = caught.errors ? caught.errors.join("\n") : String(caught);
    error.hidden = false;
  };

  const fileInput = el("input", {
    type: "file",
    accept: `${PROJECT_EXTENSION},.json,application/json`,
    hidden: true,
    onchange: async (event) => {
      const [file] = event.target.files ?? [];
      event.target.value = "";
      if (file) openProjectFile(file).catch(report);
    },
  });

  const urlInput = el("input", {
    class: "coordinate",
    type: "url",
    spellcheck: false,
    placeholder: `projects/…${PROJECT_EXTENSION}`,
  });

  const bundled = el("div", { class: "places" });
  loadManifest().then((entries) => {
    bundled.replaceChildren(
      ...entries.map((entry) =>
        button(entry.name, () => openProjectUrl(`projects/${entry.file}`), {
          title: entry.description ?? "",
        }),
      ),
    );
  });

  const save = button("Save…", () => {
    if (lastState) downloadProject(captureProject(lastState, project));
  });
  bind(save, (state) => {
    lastState = state;
  });

  return section(
    "Project",
    el("p", { class: "project-name" }, project.name ?? "Untitled"),
    project.description ? el("p", { class: "detail" }, project.description) : null,
    el("p", { class: "detail" }, source ?? "not saved"),
    el(
      "div",
      { class: "buttons" },
      button("Open file…", () => fileInput.click()),
      save,
      button("New", openEmptyProject),
    ),
    fileInput,
    row("URL", urlInput),
    el(
      "div",
      { class: "buttons" },
      button("Open URL", () => {
        const url = urlInput.value.trim();
        if (url) openProjectUrl(url);
      }),
    ),
    el("p", { class: "detail" }, "Examples"),
    bundled,
    error,
  );
}
