(() => {
  const scrollKey = "route-llm-admin-scroll-y";
  const savedScroll = sessionStorage.getItem(scrollKey);
  if (savedScroll !== null) {
    sessionStorage.removeItem(scrollKey);
    requestAnimationFrame(() => window.scrollTo({ top: Number(savedScroll), behavior: "auto" }));
  }

  let dragged = null;

  const openTokenModal = (clientId) => {
    const modal = document.querySelector(`[data-token-modal="${clientId}"]`);
    if (!modal) return;
    if (modal.parentElement !== document.body) {
      document.body.appendChild(modal);
    }
    modal.hidden = false;
    document.body.classList.add("modal-open");
    modal.querySelector("input[name='name']")?.focus();
  };

  const closeTokenModal = (modal) => {
    if (!modal) return;
    modal.hidden = true;
    document.body.classList.remove("modal-open");
  };

  const copyText = async (value) => {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      try {
        await navigator.clipboard.writeText(value);
        return;
      } catch (_) {
      }
    }
    const field = document.createElement("textarea");
    field.value = value;
    field.setAttribute("readonly", "");
    field.style.position = "fixed";
    field.style.left = "-9999px";
    field.style.top = "0";
    document.body.appendChild(field);
    field.focus();
    field.select();
    document.execCommand("copy");
    field.remove();
  };

  const saveAndReload = async (action, values) => {
    sessionStorage.setItem(scrollKey, String(window.scrollY));
    const response = await fetch(action, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams(values),
      credentials: "same-origin"
    });
    if (response.redirected && response.url) {
      window.location.href = response.url;
      return;
    }
    if (response.ok) {
      window.location.reload();
      return;
    }
    window.location.href = "/admin?error=" + encodeURIComponent("요청을 처리하지 못했습니다");
  };

  const replaceModelField = (card, models) => {
    const field = card.querySelector("[data-model-name-field]");
    if (!field || !models.length) return;

    const previousValue = field.querySelector("[name='model']")?.value || "";
    const registered = new Set(
      Array.from(card.querySelectorAll("[data-registered-model]"))
        .map((item) => item.dataset.registeredModel || "")
        .filter(Boolean)
    );
    const select = document.createElement("select");
    select.name = "model";
    select.required = true;
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "실제 모델 선택";
    select.appendChild(placeholder);
    models.forEach((model) => {
      const option = document.createElement("option");
      option.value = model;
      option.textContent = registered.has(model) ? `${model} (등록됨)` : model;
      if (model === previousValue) option.selected = true;
      select.appendChild(option);
    });
    field.replaceChildren("실제 모델명 ", select);
  };

  const refreshProviderModels = async () => {
    const cards = Array.from(document.querySelectorAll("[data-upstream-id]"));
    await Promise.all(cards.map(async (card) => {
      const upstreamId = card.dataset.upstreamId;
      const meta = card.querySelector("[data-model-fetch-meta]");
      if (!upstreamId) return;
      if (meta) meta.textContent = "실제 모델 목록 확인 중...";
      try {
        const response = await fetch("/admin/upstreams/fetch-models", {
          method: "POST",
          headers: {
            "Accept": "application/json",
            "Content-Type": "application/x-www-form-urlencoded"
          },
          body: new URLSearchParams({ id: upstreamId }),
          credentials: "same-origin"
        });
        if (!response.ok) {
          const message = response.headers.get("content-type")?.includes("application/json")
            ? (await response.json()).error
            : "모델 목록을 가져오지 못했습니다";
          throw new Error(message || "모델 목록을 가져오지 못했습니다");
        }
        const result = await response.json();
        const models = Array.isArray(result.models) ? result.models : [];
        replaceModelField(card, models);
        if (meta) meta.textContent = `가져온 모델 ${models.length}개 · 방금 갱신`;
      } catch (error) {
        if (meta) meta.textContent = error.message || "모델 목록을 가져오지 못했습니다";
      }
    }));
  };

  const closeAutocompleteMenus = (except) => {
    document.querySelectorAll(".autocomplete-menu.open").forEach((menu) => {
      if (menu !== except) menu.classList.remove("open");
    });
  };

  document.querySelectorAll("[data-alias-autocomplete]").forEach((input) => {
    const field = input.closest(".autocomplete-field");
    const menu = field ? field.querySelector("[data-alias-options]") : null;
    if (!field || !menu) return;

    const syncMenu = () => {
      const query = input.value.trim().toLowerCase();
      let visible = 0;
      menu.querySelectorAll("[data-autocomplete-value]").forEach((option) => {
        const value = (option.dataset.autocompleteValue || "").toLowerCase();
        const matched = !query || value.includes(query);
        option.hidden = !matched;
        if (matched) visible += 1;
      });
      closeAutocompleteMenus(menu);
      menu.classList.toggle("open", visible > 0);
    };

    input.addEventListener("focus", syncMenu);
    input.addEventListener("click", syncMenu);
    input.addEventListener("input", syncMenu);
    menu.addEventListener("mousedown", (event) => event.preventDefault());
    menu.addEventListener("click", (event) => {
      const option = event.target.closest("[data-autocomplete-value]");
      if (!option) return;
      input.value = option.dataset.autocompleteValue || "";
      menu.classList.remove("open");
      input.focus();
    });
  });

  document.addEventListener("click", (event) => {
    if (!event.target.closest(".autocomplete-field")) {
      closeAutocompleteMenus(null);
    }
  });

  document.querySelectorAll("[data-sort-item]").forEach((item) => {
    item.addEventListener("dragstart", (event) => {
      item.classList.add("dragging");
      dragged = {
        kind: "sort-item",
        scope: item.dataset.sortScope,
        element: item
      };
      if (event.dataTransfer) {
        event.dataTransfer.effectAllowed = "move";
        event.dataTransfer.setData("text/plain", item.dataset.sortId || "");
      }
    });
    item.addEventListener("dragend", () => {
      item.classList.remove("dragging");
      dragged = null;
    });
  });

  document.querySelectorAll("[data-sortable]").forEach((list) => {
    list.addEventListener("dragover", (event) => {
      if (!isDraggedWithin(list)) return;
      event.preventDefault();
      const afterElement = getDragAfterElement(list, event.clientY);
      if (!afterElement) {
        list.appendChild(dragged.element);
      } else {
        list.insertBefore(dragged.element, afterElement);
      }
    });
    list.addEventListener("drop", async (event) => {
      if (!isDraggedWithin(list)) return;
      event.preventDefault();
      const ids = Array.from(list.querySelectorAll("[data-sort-item]"))
        .map((item) => item.dataset.sortId)
        .filter(Boolean)
        .join(",");
      if (ids) {
        const values = { ids };
        const scopeParts = (list.dataset.sortScope || "").split(":");
        if (scopeParts[0] === "client") {
          values.client_id = scopeParts[1];
        } else if (scopeParts[0] === "keys") {
          values.provider_id = scopeParts[1];
        }
        await saveAndReload(list.dataset.reorderAction, values);
      }
    });
  });

  const getDragAfterElement = (container, y) => {
    const items = [...container.querySelectorAll("[data-sort-item]:not(.dragging)")];
    return items.reduce((closest, child) => {
      const box = child.getBoundingClientRect();
      const offset = y - box.top - box.height / 2;
      if (offset < 0 && offset > closest.offset) {
        return { offset, element: child };
      }
      return closest;
    }, { offset: Number.NEGATIVE_INFINITY, element: null }).element;
  };

  const isDraggedWithin = (list) => (
    dragged &&
    dragged.kind === "sort-item" &&
    dragged.scope === list.dataset.sortScope
  );

  document.querySelectorAll("[data-open-token-modal]").forEach((button) => {
    button.addEventListener("click", () => openTokenModal(button.dataset.openTokenModal));
  });

  document.querySelectorAll("[data-close-token-modal]").forEach((button) => {
    button.addEventListener("click", () => closeTokenModal(button.closest("[data-token-modal]")));
  });

  document.querySelectorAll("[data-token-modal]").forEach((modal) => {
    modal.addEventListener("click", (event) => {
      if (event.target === modal) closeTokenModal(modal);
    });
  });

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    document.querySelectorAll("[data-token-modal]").forEach((modal) => {
      if (!modal.hidden) closeTokenModal(modal);
    });
  });

  document.querySelectorAll("[data-copy-token-value]").forEach((button) => {
    button.addEventListener("click", async () => {
      await copyText(button.dataset.copyTokenValue || "");
      const label = button.querySelector("span");
      const original = label ? label.textContent : button.textContent;
      if (label) {
        label.textContent = "복사됨";
      } else {
        button.textContent = "복사됨";
      }
      setTimeout(() => {
        if (label) {
          label.textContent = original || "토큰 복사";
        } else {
          button.textContent = original || "토큰 복사";
        }
      }, 1200);
    });
  });

  const tokenClient = new URLSearchParams(window.location.search).get("token_client");
  if (tokenClient) {
    requestAnimationFrame(() => openTokenModal(tokenClient));
  }

  refreshProviderModels();
})();
