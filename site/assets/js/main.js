/* Ecat static site: language switch, menus, tabs, copy, mascot, back-to-top. */
(() => {
  window.__ecatReady = true;
  const EN = window.ECAT_EN || {};
  const body = document.body;
  const root = document.documentElement;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  const UI = {
    menuOpen: { zh: "打开菜单", en: "Open menu" },
    menuClose: { zh: "关闭菜单", en: "Close menu" },
    backTop: { zh: "回到顶部", en: "Back to top" },
    copy: { zh: "复制", en: "Copy" },
    copied: { zh: "已复制", en: "Copied" },
    copyToast: { zh: "命令已复制", en: "Command copied" },
    copyFailed: { zh: "已选中代码，请按 Ctrl/Cmd+C 复制", en: "Code selected. Press Ctrl/Cmd+C to copy." },
    petToast: { zh: "返回顶部宠物已更新", en: "Back-to-top pet updated" },
    selectPet: { zh: "选择这个形态", en: "Select this mascot form" },
  };

  const storage = {
    get(key) {
      try {
        return localStorage.getItem(key);
      } catch {
        return null;
      }
    },
    set(key, value) {
      try {
        localStorage.setItem(key, value);
      } catch {
        /* Storage may be blocked in private mode. */
      }
    },
  };

  const PETS = {
    ecat: { img: "assets/img/e-cat.svg", name: "pet.name.main", zh: "主形象" },
    smile: { img: "assets/img/pet/smile.svg", name: "pet.name.smile", zh: "笑" },
    joy: { img: "assets/img/pet/joy.svg", name: "pet.name.joy", zh: "欢乐" },
    cry: { img: "assets/img/pet/cry.svg", name: "pet.name.cry", zh: "哭" },
    depressed: { img: "assets/img/pet/depressed.svg", name: "pet.name.depressed", zh: "沮丧" },
    terrified: { img: "assets/img/pet/terrified.svg", name: "pet.name.terrified", zh: "惊恐" },
    angry: { img: "assets/img/pet/angry.svg", name: "pet.name.angry", zh: "愤怒" },
    coquettish: { img: "assets/img/pet/coquettish.svg", name: "pet.name.coquettish", zh: "撒娇" },
    cute: { img: "assets/img/pet/cute.svg", name: "pet.name.cute", zh: "卖萌" },
    pleasing: { img: "assets/img/pet/pleasing.svg", name: "pet.name.pleasing", zh: "讨好" },
    knocking: { img: "assets/img/pet/knocking.svg", name: "pet.name.knocking", zh: "敲打" },
    facepalm: { img: "assets/img/pet/facepalm.svg", name: "pet.name.facepalm", zh: "捂脸" },
    tongue: { img: "assets/img/pet/tongue.svg", name: "pet.name.tongue", zh: "吐舌头" },
  };

  let lang = "zh";
  let petSlug = "smile";

  const originals = new Map();
  document
    .querySelectorAll("[data-i18n], [data-i18n-html], [data-i18n-aria]")
    .forEach((el) => {
      originals.set(el, {
        html: el.innerHTML,
        aria: el.getAttribute("aria-label"),
      });
    });

  const t = (key) => (lang === "en" ? UI[key].en : UI[key].zh);

  function applyLang(next) {
    lang = next === "en" ? "en" : "zh";
    root.lang = lang === "en" ? "en" : "zh-CN";

    document
      .querySelectorAll("[data-i18n], [data-i18n-html], [data-i18n-aria]")
      .forEach((el) => {
        const key = el.dataset.i18n || el.dataset.i18nHtml;
        const original = originals.get(el);
        if (!original) return;

        if (lang === "en") {
          if (key && EN[key] != null) el.innerHTML = EN[key];
          if (el.dataset.i18nAria && EN[el.dataset.i18nAria]) {
            el.setAttribute("aria-label", EN[el.dataset.i18nAria]);
          }
        } else {
          if (key) el.innerHTML = original.html;
          if (el.dataset.i18nAria) {
            if (original.aria == null) el.removeAttribute("aria-label");
            else el.setAttribute("aria-label", original.aria);
          }
        }
      });

    const title = lang === "en" ? body.dataset.titleEn : body.dataset.titleZh;
    const desc = lang === "en" ? body.dataset.descEn : body.dataset.descZh;
    if (title) document.title = title;
    if (desc) {
      const meta = document.querySelector('meta[name="description"]');
      if (meta) meta.setAttribute("content", desc);
    }

    const toggle = document.getElementById("lang-toggle");
    if (toggle) {
      toggle.textContent = lang === "en" ? "中" : "EN";
      toggle.setAttribute("aria-label", lang === "en" ? "切换到中文" : "Switch to English");
      toggle.setAttribute("aria-pressed", lang === "en" ? "true" : "false");
    }

    const menuToggle = document.getElementById("menu-toggle");
    if (menuToggle) {
      const open = menuToggle.getAttribute("aria-expanded") === "true";
      menuToggle.setAttribute("aria-label", t(open ? "menuClose" : "menuOpen"));
    }

    const backTop = document.getElementById("back-top");
    if (backTop) backTop.setAttribute("aria-label", t("backTop"));

    updatePetPreview();
    storage.set("ecat-lang", lang);
    document.dispatchEvent(new CustomEvent("ecat:lang", { detail: { lang } }));
  }

  function showToast(message) {
    const toast = document.getElementById("toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("is-visible");
    window.clearTimeout(showToast.timer);
    showToast.timer = window.setTimeout(() => {
      toast.classList.remove("is-visible");
    }, 2200);
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      const area = document.createElement("textarea");
      area.value = text;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      let ok = false;
      try {
        ok = document.execCommand("copy");
      } catch {
        ok = false;
      }
      area.remove();
      return ok;
    }
  }

  /* Language */

  const storedLang = storage.get("ecat-lang");
  const langToggle = document.getElementById("lang-toggle");
  if (langToggle) {
    langToggle.addEventListener("click", () => {
      applyLang(lang === "en" ? "zh" : "en");
    });
  }

  /* Mobile navigation */

  const menuToggle = document.getElementById("menu-toggle");
  const nav = document.getElementById("site-nav");
  if (menuToggle && nav) {
    const setMenu = (open) => {
      nav.classList.toggle("is-open", open);
      menuToggle.setAttribute("aria-expanded", String(open));
      menuToggle.setAttribute("aria-label", t(open ? "menuClose" : "menuOpen"));
      menuToggle.innerHTML =
        '<svg class="icon" aria-hidden="true"><use href="assets/icons/sprite.svg#i-' +
        (open ? "x" : "menu") +
        '"></use></svg>';
    };
    menuToggle.addEventListener("click", () => {
      setMenu(!nav.classList.contains("is-open"));
    });
    nav.addEventListener("click", (event) => {
      if (event.target.closest("a")) setMenu(false);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && nav.classList.contains("is-open")) {
        setMenu(false);
        menuToggle.focus();
      }
    });
  }

  /* Copy buttons */

  document.querySelectorAll(".copy-btn").forEach((button) => {
    button.addEventListener("click", async () => {
      const target = document.querySelector(button.dataset.copy);
      if (!target) return;
      const ok = await copyText(target.innerText);
      if (!ok) {
        const range = document.createRange();
        range.selectNodeContents(target);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        showToast(t("copyFailed"));
        return;
      }
      const label = button.querySelector(".copy-label");
      button.classList.add("is-copied");
      if (label) label.textContent = t("copied");
      showToast(t("copyToast"));
      window.setTimeout(() => {
        button.classList.remove("is-copied");
        if (label) label.textContent = t("copy");
      }, 1600);
    });
  });

  /* Defer images inside closed <details> until opened. */

  document.querySelectorAll("details").forEach((details) => {
    const loadDeferred = () => {
      details.querySelectorAll("img[data-src]").forEach((img) => {
        img.src = img.dataset.src;
        img.removeAttribute("data-src");
      });
    };
    details.addEventListener("toggle", () => {
      if (details.open) loadDeferred();
    });
    if (details.open) loadDeferred();
  });

  /* Tabs */

  const tabButtons = Array.from(document.querySelectorAll('[role="tab"]'));
  function selectTab(tab, focus = false) {
    const list = tab.closest('[role="tablist"]');
    if (!list) return;
    const tabs = Array.from(list.querySelectorAll('[role="tab"]'));
    tabs.forEach((item) => {
      const selected = item === tab;
      item.setAttribute("aria-selected", String(selected));
      item.tabIndex = selected ? 0 : -1;
      const panel = document.getElementById(item.getAttribute("aria-controls"));
      if (panel) panel.hidden = !selected;
    });
    if (focus) tab.focus();
  }
  tabButtons.forEach((tab) => {
    tab.addEventListener("click", () => selectTab(tab));
    tab.addEventListener("keydown", (event) => {
      const list = tab.closest('[role="tablist"]');
      if (!list) return;
      const tabs = Array.from(list.querySelectorAll('[role="tab"]'));
      const index = tabs.indexOf(tab);
      let next = null;
      if (event.key === "ArrowRight" || event.key === "ArrowDown") {
        next = tabs[(index + 1) % tabs.length];
      } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
        next = tabs[(index - 1 + tabs.length) % tabs.length];
      } else if (event.key === "Home") {
        next = tabs[0];
      } else if (event.key === "End") {
        next = tabs[tabs.length - 1];
      }
      if (next) {
        event.preventDefault();
        selectTab(next, true);
      }
    });
  });
  if (tabButtons.length) {
    const initial =
      tabButtons.find((tab) => tab.getAttribute("aria-selected") === "true") || tabButtons[0];
    selectTab(initial);
  }

  /* Mascot selection and back-to-top */

  const backTop = document.getElementById("back-top");
  const backTopPet = document.getElementById("back-top-pet");
  const previewImg = document.getElementById("pet-preview-img");
  const previewName = document.getElementById("pet-preview-name");
  const previewCode = document.getElementById("pet-preview-code");

  function petName(slug) {
    const pet = PETS[slug] || PETS.smile;
    if (lang === "en" && EN[pet.name]) return EN[pet.name];
    return pet.zh;
  }

  function updatePetPreview() {
    const pet = PETS[petSlug] || PETS.smile;
    if (backTopPet) backTopPet.src = pet.img;
    if (previewImg) {
      previewImg.src = pet.img;
      previewImg.alt = petName(petSlug);
    }
    if (previewName) previewName.textContent = petName(petSlug);
    if (previewCode) previewCode.textContent = "ecat pet show " + petSlug;
    document.querySelectorAll(".pet-card").forEach((card) => {
      const selected = card.dataset.slug === petSlug;
      card.classList.toggle("is-selected", selected);
      card.setAttribute("aria-pressed", String(selected));
      card.setAttribute("aria-label", t("selectPet") + ": " + petName(card.dataset.slug));
    });
  }

  function selectPet(slug, notify) {
    if (!PETS[slug]) return;
    petSlug = slug;
    storage.set("ecat-pet", slug);
    updatePetPreview();
    if (notify) showToast(t("petToast"));
  }

  const storedPet = storage.get("ecat-pet");
  if (storedPet && PETS[storedPet]) petSlug = storedPet;

  document.querySelectorAll(".pet-card").forEach((card) => {
    card.addEventListener("click", () => selectPet(card.dataset.slug, true));
  });
  updatePetPreview();

  if (backTop) {
    backTop.addEventListener("click", () => {
      window.scrollTo({ top: 0, behavior: reduced ? "auto" : "smooth" });
      if (backTopPet && petSlug !== "joy") {
        const before = backTopPet.src;
        backTopPet.src = "assets/img/pet/joy.svg";
        window.setTimeout(() => {
          backTopPet.src = before;
        }, 900);
      }
    });

    const sentinel = document.getElementById("back-top-sentinel");
    if (sentinel && "IntersectionObserver" in window) {
      const observer = new IntersectionObserver(
        ([entry]) => {
          backTop.classList.toggle("is-visible", entry.boundingClientRect.top < 0);
        },
        { threshold: 0 },
      );
      observer.observe(sentinel);
    }
  }

  /* Donation QR dialog */

  const qrDialog = document.getElementById("qr-dialog");
  const qrDialogImg = document.getElementById("qr-dialog-img");
  const qrDialogTitle = document.getElementById("qr-dialog-title");
  let qrTrigger = null;

  function updateQrText() {
    if (!qrTrigger) return;
    qrDialogTitle.textContent = lang === "en" ? qrTrigger.dataset.qrEn : qrTrigger.dataset.qrZh;
    qrDialogImg.alt = lang === "en" ? qrTrigger.dataset.qrAltEn : qrTrigger.dataset.qrAltZh;
  }

  function openQr(trigger) {
    qrTrigger = trigger;
    qrDialogImg.src = trigger.dataset.qrImg;
    qrDialogImg.width = trigger.querySelector("img").naturalWidth || 320;
    qrDialogImg.height = trigger.querySelector("img").naturalHeight || 405;
    updateQrText();
    if (typeof qrDialog.showModal === "function") {
      qrDialog.showModal();
    } else {
      qrDialog.setAttribute("open", "");
    }
    document.documentElement.classList.add("has-qr-dialog");
  }

  function onQrClosed() {
    document.documentElement.classList.remove("has-qr-dialog");
    if (qrTrigger && document.contains(qrTrigger)) qrTrigger.focus();
    qrTrigger = null;
  }

  function closeQr() {
    if (typeof qrDialog.close === "function" && qrDialog.open) {
      qrDialog.close();
    } else {
      qrDialog.removeAttribute("open");
      onQrClosed();
    }
  }

  if (qrDialog && qrDialogImg && qrDialogTitle) {
    document.querySelectorAll("[data-qr]").forEach((trigger) => {
      trigger.addEventListener("click", () => openQr(trigger));
    });
    qrDialog.addEventListener("close", onQrClosed);
    qrDialog.addEventListener("click", (event) => {
      if (event.target === qrDialog) closeQr();
    });
    const closeButton = qrDialog.querySelector("[data-qr-close]");
    if (closeButton) closeButton.addEventListener("click", closeQr);
    if (typeof qrDialog.showModal !== "function") {
      document.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && qrDialog.hasAttribute("open")) closeQr();
      });
    }
    document.addEventListener("ecat:lang", () => {
      if (qrDialog.open) updateQrText();
    });
  }

  /* Reveal on scroll */

  const revealItems = document.querySelectorAll(".reveal");
  if (!reduced && "IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            entry.target.classList.add("is-visible");
            observer.unobserve(entry.target);
          }
        });
      },
      { threshold: 0.12, rootMargin: "0px 0px -40px 0px" },
    );
    revealItems.forEach((item) => observer.observe(item));
  } else {
    revealItems.forEach((item) => item.classList.add("is-visible"));
  }

  /* Three.js hero: no image fallback, so load the animation right away. */

  const saveData = navigator.connection && navigator.connection.saveData;
  if (document.getElementById("cat-canvas") && !reduced && !saveData) {
    const url = new URL("assets/js/three-scene.js", document.baseURI).href;
    import(url).catch(() => {});
  }

  /* Visitor counter: load after the page so a slow third party cannot delay it. */

  function loadCounter() {
    const script = document.createElement("script");
    script.async = true;
    script.src = "https://busuanzi.ibruce.info/busuanzi/2.3/busuanzi.pure.mini.js";
    document.body.appendChild(script);
  }

  window.addEventListener(
    "load",
    () => {
      if ("requestIdleCallback" in window) {
        window.requestIdleCallback(loadCounter, { timeout: 2000 });
      } else {
        loadCounter();
      }
    },
    { once: true },
  );

  applyLang(storedLang === "en" ? "en" : "zh");
})();
