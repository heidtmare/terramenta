/**
 * Project section: open project, unsaved state, autosave recovery, recent and
 * bundled projects, open (file/URL/drop), save. Opening switches in place —
 * see `workspace.js`.
 */

import { el, row, section } from "./dom.js";
import { PROJECT_EXTENSION, loadManifest } from "./project.js";
import { button } from "./widgets.js";
import * as workspace from "./workspace.js";

/** `signal` aborts when the project closes. */
export function mountProjectSection({ signal }) {
  const error = el("p", { class: "detail error", hidden: true });
  const report = (caught) => {
    console.error(caught);
    error.textContent = caught.errors ? caught.errors.join("\n") : String(caught);
    error.hidden = false;
  };
  /** Runs an open; errors land in `error`. */
  const run = (opening) => {
    error.hidden = true;
    Promise.resolve()
      .then(opening)
      .catch(report);
  };
  workspace.onDropError(report);

  const fileInput = el("input", {
    type: "file",
    accept: `${PROJECT_EXTENSION},.json,application/json`,
    hidden: true,
    onchange: (event) => {
      const [file] = event.target.files ?? [];
      event.target.value = "";
      if (file) run(() => workspace.openFile(file));
    },
  });

  const urlInput = el("input", {
    class: "coordinate",
    type: "url",
    spellcheck: false,
    placeholder: `projects/…${PROJECT_EXTENSION}`,
  });

  const name = el("p", { class: "project-name" });
  const description = el("p", { class: "detail" });
  const source = el("p", { class: "detail" });
  const recovery = el("div", { class: "recovery" });
  const recent = el("div", { class: "places" });
  const recentBlock = el("div", {}, el("p", { class: "detail" }, "Recent"), recent);

  const bundled = el("div", { class: "places" });
  loadManifest().then((entries) => {
    bundled.replaceChildren(
      ...entries.map((entry) =>
        button(entry.name, () => run(() => workspace.openUrl(`projects/${entry.file}`)), {
          title: entry.description ?? "",
        }),
      ),
    );
  });

  const render = () => {
    const current = workspace.snapshot();
    name.textContent = `${current.project.name ?? "Untitled"}${current.dirty ? " •" : ""}`;
    name.title = current.dirty ? "Unsaved changes" : "";
    description.textContent = current.project.description ?? "";
    description.hidden = !current.project.description;
    source.textContent = `${current.source ?? "not saved"}${current.dirty ? " · unsaved changes" : ""}`;

    recovery.replaceChildren();
    if (current.recovery) {
      const { project, savedAt } = current.recovery;
      recovery.append(
        el(
          "p",
          { class: "detail" },
          `Unsaved work from ${new Date(savedAt).toLocaleString()}: ${project.name ?? "Untitled"}`,
        ),
        el(
          "div",
          { class: "buttons" },
          button("Recover", () => run(workspace.recover)),
          button("Dismiss", workspace.dismissRecovery),
        ),
      );
    }

    const names = current.recent.map((entry) => entry.name);
    recent.replaceChildren(
      ...current.recent.map((entry) => {
        const shared = names.indexOf(entry.name) !== names.lastIndexOf(entry.name);
        const label = shared ? `${entry.name} · ${entry.source.split("/").pop()}` : entry.name;
        return button(label, () => run(() => workspace.openRecent(entry)), { title: entry.source });
      }),
    );
    recentBlock.hidden = current.recent.length === 0;
  };
  render();
  const unsubscribe = workspace.subscribe(render);
  signal?.addEventListener("abort", unsubscribe);

  return section(
    "Project",
    name,
    description,
    source,
    el(
      "div",
      { class: "buttons" },
      button("Open file…", () => fileInput.click()),
      button("Save…", workspace.save),
      button("New", () => run(workspace.openEmpty)),
    ),
    fileInput,
    recovery,
    row("URL", urlInput),
    el(
      "div",
      { class: "buttons" },
      button("Open URL", () => {
        const url = urlInput.value.trim();
        if (url) run(() => workspace.openUrl(url));
      }),
    ),
    recentBlock,
    el("p", { class: "detail" }, "Examples"),
    bundled,
    el("p", { class: "detail" }, `Or drop a ${PROJECT_EXTENSION} file anywhere.`),
    error,
  );
}
