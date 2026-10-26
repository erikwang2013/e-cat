/* Ecat static site: client-side search over the content pages. No build step. */
(() => {
  const EN = window.ECAT_EN || {};
  const root = document.documentElement;
  const LIMIT = 12;

  const PAGES = [
    { file: "index.html", url: "./", key: "search.page.home", zh: "概览" },
    { file: "docs.html", url: "docs.html", key: "search.page.docs", zh: "文档" },
    { file: "changelog.html", url: "changelog.html", key: "search.page.changelog", zh: "版本日志" },
    { file: "pet.html", url: "pet.html", key: "search.page.pet", zh: "项目宠物" },
  ];

  const UI = {
    open: { zh: "搜索站点", en: "Search the site" },
    title: { zh: "搜索", en: "Search" },
    placeholder: { zh: "搜索文档、版本与页面…", en: "Search docs, releases, and pages…" },
    close: { zh: "关闭搜索", en: "Close search" },
    results: { zh: "搜索结果", en: "Search results" },
    hint: {
      zh: "输入关键词开始搜索；Ctrl/Cmd+K 或 / 可随时打开。",
      en: "Type to search. Ctrl/Cmd+K or / opens this anytime.",
    },
    loading: { zh: "正在建立索引…", en: "Building the index…" },
    empty: { zh: "没有找到匹配结果。", en: "No matching results." },
    error: { zh: "搜索暂时不可用，请稍后再试。", en: "Search is unavailable right now. Try again later." },
    count: { zh: "找到 {n} 条结果", en: "{n} results" },
  };

  function storageLang() {
    try {
      return localStorage.getItem("ecat-lang");
    } catch {
      return null;
    }
  }

  const currentLang = () =>
    root.lang.toLowerCase().startsWith("en") || storageLang() === "en" ? "en" : "zh";
  const t = (key) => UI[key][currentLang()];
  const pageName = (page) => (currentLang() === "en" && EN[page.key]) || page.zh;

  /* Dialog and search button */

  const dialog = document.createElement("dialog");
  dialog.className = "search-dialog";
  dialog.id = "ecat-search";
  dialog.setAttribute("aria-labelledby", "ecat-search-title");
  dialog.innerHTML = `
    <div class="search-box">
      <div class="search-head">
        <svg class="icon" aria-hidden="true"><use href="assets/icons/sprite.svg#i-search"></use></svg>
        <h2 class="sr-only" id="ecat-search-title"></h2>
        <input class="search-input" id="ecat-search-input" type="search" role="combobox"
          aria-expanded="true" aria-autocomplete="list" aria-controls="ecat-search-results"
          autocomplete="off" spellcheck="false">
        <button class="search-close" type="button">
          <svg class="icon icon-sm" aria-hidden="true"><use href="assets/icons/sprite.svg#i-x"></use></svg>
        </button>
      </div>
      <p class="search-status" role="status" aria-live="polite"></p>
      <ul class="search-results" id="ecat-search-results" role="listbox"></ul>
    </div>`;
  document.body.appendChild(dialog);

  const input = dialog.querySelector(".search-input");
  const status = dialog.querySelector(".search-status");
  const list = dialog.querySelector(".search-results");
  const heading = dialog.querySelector("#ecat-search-title");
  const closeButton = dialog.querySelector(".search-close");

  let toggle = null;
  let lastFocus = null;
  let rawPages = null;
  let loadPromise = null;
  let entries = [];
  let indexLang = null;
  let results = [];
  let active = -1;
  let inputTimer = 0;

  function updateLabels() {
    heading.textContent = t("title");
    input.setAttribute("aria-label", t("open"));
    input.placeholder = t("placeholder");
    list.setAttribute("aria-label", t("results"));
    closeButton.setAttribute("aria-label", t("close"));
    if (toggle) toggle.setAttribute("aria-label", t("open"));
  }

  function mountToggle() {
    const actions = document.querySelector(".site-header .header-actions");
    if (!actions) return;
    toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "icon-btn search-toggle";
    toggle.innerHTML =
      '<svg class="icon" aria-hidden="true"><use href="assets/icons/sprite.svg#i-search"></use></svg>';
    toggle.addEventListener("click", openDialog);
    actions.prepend(toggle);
    updateLabels();
  }

  /* Indexing: fetch each page once, translate a detached copy, then read headings + text. */

  const SKIP = new Set([
    "SCRIPT",
    "STYLE",
    "SVG",
    "NAV",
    "HEADER",
    "FOOTER",
    "DIALOG",
    "BUTTON",
    "TEMPLATE",
    "NOSCRIPT",
  ]);

  function translateDoc(doc) {
    doc.querySelectorAll("[data-i18n], [data-i18n-html]").forEach((el) => {
      const key = el.getAttribute("data-i18n") || el.getAttribute("data-i18n-html");
      const value = key ? EN[key] : null;
      if (value != null) el.innerHTML = value;
    });
  }

  function headingText(el) {
    return Array.from(el.childNodes)
      .filter((node) => node.nodeType === 1 || node.nodeType === 3)
      .map((node) => node.textContent)
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();
  }

  function extract(doc, page) {
    const main = doc.querySelector("main") || doc.body;
    const out = [];
    let current = null;

    function walk(el) {
      for (const node of el.childNodes) {
        if (node.nodeType === 3) {
          if (current) {
            const text = node.nodeValue.replace(/\s+/g, " ").trim();
            if (text) current.text += " " + text;
          }
          continue;
        }
        if (node.nodeType !== 1 || SKIP.has(node.tagName)) continue;
        if (/^H[1-4]$/.test(node.tagName) || node.tagName === "SUMMARY") {
          const title = headingText(node);
          if (title) {
            const holder = node.closest("[id]");
            const anchor = holder && holder !== main && holder.id !== "main" ? holder.id : "";
            current = { page, url: page.url, title, anchor, text: "" };
            out.push(current);
          }
          continue;
        }
        walk(node);
      }
    }

    walk(main);
    return out;
  }

  function buildEntries(htmls) {
    const english = currentLang() === "en";
    const all = [];
    for (const page of PAGES) {
      const html = htmls.get(page.file);
      if (!html) continue;
      const doc = new DOMParser().parseFromString(html, "text/html");
      if (english) translateDoc(doc);
      all.push(...extract(doc, page));
    }
    return all;
  }

  async function ensureIndex() {
    if (!rawPages) {
      if (!loadPromise) {
        loadPromise = (async () => {
          const htmls = new Map();
          await Promise.all(
            PAGES.map(async (page) => {
              const response = await fetch(page.file, { credentials: "same-origin" });
              if (!response.ok) throw new Error(page.file + ": HTTP " + response.status);
              htmls.set(page.file, await response.text());
            }),
          );
          rawPages = htmls;
        })().finally(() => {
          loadPromise = null;
        });
      }
      await loadPromise;
    }
    if (indexLang !== currentLang()) {
      entries = buildEntries(rawPages);
      indexLang = currentLang();
    }
  }

  /* Query and rendering */

  function snippet(text, terms) {
    const lower = text.toLowerCase();
    let at = -1;
    for (const term of terms) {
      const found = lower.indexOf(term);
      if (found >= 0 && (at < 0 || found < at)) at = found;
    }
    if (at < 0) at = 0;
    const start = Math.max(0, at - 60);
    const end = Math.min(text.length, start + 160);
    return (start > 0 ? "…" : "") + text.slice(start, end).trim() + (end < text.length ? "…" : "");
  }

  function search(query) {
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return [];
    const hits = [];
    for (const entry of entries) {
      const title = entry.title.toLowerCase();
      const text = entry.text.toLowerCase();
      let score = 0;
      let matches = true;
      for (const term of terms) {
        const inTitle = title.includes(term);
        const inText = text.includes(term);
        if (!inTitle && !inText) {
          matches = false;
          break;
        }
        score += (inTitle ? 4 : 0) + (inText ? 1 : 0);
      }
      if (matches) hits.push({ entry, score });
    }
    hits.sort((a, b) => b.score - a.score || a.entry.title.length - b.entry.title.length);
    return hits.slice(0, LIMIT).map(({ entry }) => ({
      ...entry,
      snippet: snippet(entry.text.trim(), terms),
    }));
  }

  function setActive(next) {
    if (!results.length) return;
    active = Math.max(0, Math.min(next, results.length - 1));
    Array.from(list.children).forEach((item, i) => {
      const selected = i === active;
      item.setAttribute("aria-selected", String(selected));
      if (selected) {
        input.setAttribute("aria-activedescendant", item.id);
        item.scrollIntoView({ block: "nearest" });
      }
    });
  }

  function go(entry) {
    dialog.close();
    const url = new URL(entry.url, document.baseURI);
    if (entry.anchor) url.hash = entry.anchor;
    location.href = url.href;
  }

  function render(query) {
    if (!rawPages) return;
    const q = query.trim();
    results = q ? search(q) : [];
    active = -1;
    list.textContent = "";
    input.removeAttribute("aria-activedescendant");
    status.textContent = !q ? t("hint") : results.length ? t("count").replace("{n}", results.length) : t("empty");

    results.forEach((entry, i) => {
      const item = document.createElement("li");
      item.className = "search-result";
      item.id = "ecat-search-opt-" + i;
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", "false");

      const title = document.createElement("p");
      title.className = "search-result-title";
      title.textContent = entry.title;

      const meta = document.createElement("p");
      meta.className = "search-result-meta";
      meta.textContent = pageName(entry.page) + (entry.anchor ? " · #" + entry.anchor : "");

      const body = document.createElement("p");
      body.className = "search-result-snippet";
      body.textContent = entry.snippet;

      item.append(title, meta, body);
      item.addEventListener("click", () => go(entry));
      item.addEventListener("mousemove", () => setActive(i));
      list.appendChild(item);
    });

    if (results.length) setActive(0);
  }

  /* Open, close, and keyboard */

  async function openDialog() {
    if (dialog.open) {
      input.focus();
      return;
    }
    lastFocus = document.activeElement;
    updateLabels();
    dialog.showModal();
    document.documentElement.classList.add("has-search-dialog");
    input.focus();
    if (rawPages) {
      render(input.value);
    } else {
      results = [];
      list.textContent = "";
      status.textContent = t("loading");
    }
    try {
      await ensureIndex();
      if (dialog.open) render(input.value);
    } catch {
      if (dialog.open) {
        results = [];
        list.textContent = "";
        status.textContent = t("error");
      }
    }
  }

  dialog.addEventListener("close", () => {
    document.documentElement.classList.remove("has-search-dialog");
    if (lastFocus && document.contains(lastFocus)) lastFocus.focus();
    lastFocus = null;
  });

  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });

  closeButton.addEventListener("click", () => dialog.close());

  input.addEventListener("input", () => {
    window.clearTimeout(inputTimer);
    inputTimer = window.setTimeout(() => {
      if (dialog.open) render(input.value);
    }, 120);
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive(active + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive(active - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      window.clearTimeout(inputTimer);
      if (rawPages) render(input.value);
      if (results.length) go(results[active >= 0 ? active : 0]);
    }
  });

  document.addEventListener("keydown", (event) => {
    const target = event.target;
    const typing =
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT" ||
        target.isContentEditable);
    if ((event.key === "k" || event.key === "K") && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      openDialog();
    } else if (event.key === "/" && !typing && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault();
      openDialog();
    }
  });

  document.addEventListener("ecat:lang", () => {
    indexLang = null;
    updateLabels();
    if (!dialog.open || !rawPages) return;
    entries = buildEntries(rawPages);
    indexLang = currentLang();
    render(input.value);
  });

  updateLabels();
  mountToggle();
})();
