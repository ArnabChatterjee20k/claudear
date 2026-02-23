import { GlobalRegistrator } from "@happy-dom/global-registrator";

GlobalRegistrator.register({ url: "http://localhost/" });

// Happy DOM does not compute layout sizes, so chart containers end up at 0x0.
// Recharts treats that as an error/warning and can fail stricter test runs.
const defaultLayoutSize = {
  clientWidth: 1024,
  clientHeight: 768,
  offsetWidth: 1024,
  offsetHeight: 768,
};

for (const [key, value] of Object.entries(defaultLayoutSize)) {
  Object.defineProperty(HTMLElement.prototype, key, {
    configurable: true,
    get() {
      return value;
    },
  });
}

HTMLElement.prototype.getBoundingClientRect = function () {
  const width = this.clientWidth || defaultLayoutSize.clientWidth;
  const height = this.clientHeight || defaultLayoutSize.clientHeight;
  return {
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: width,
    bottom: height,
    width,
    height,
    toJSON() {
      return {};
    },
  } as DOMRect;
};

if (!("ResizeObserver" in globalThis)) {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  // Cast is limited to test setup to satisfy DOM typings across Bun/Happy DOM versions.
  globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;
}

// happy-dom does not update window.location when pushState/replaceState are called.
// Patch them so the router (and tests) can read window.location.pathname after navigation.
const origPush = window.history.pushState.bind(window.history);
const origReplace = window.history.replaceState.bind(window.history);

window.history.pushState = (state, title, url) => {
  origPush(state, title, url);
  if (url) {
    const u = new URL(String(url), window.location.href);
    Object.defineProperty(window.location, "pathname", { value: u.pathname, writable: true, configurable: true });
    Object.defineProperty(window.location, "href", { value: u.href, writable: true, configurable: true });
  }
};

window.history.replaceState = (state, title, url) => {
  origReplace(state, title, url);
  if (url) {
    const u = new URL(String(url), window.location.href);
    Object.defineProperty(window.location, "pathname", { value: u.pathname, writable: true, configurable: true });
    Object.defineProperty(window.location, "href", { value: u.href, writable: true, configurable: true });
  }
};
