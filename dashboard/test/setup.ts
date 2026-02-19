import { GlobalRegistrator } from "@happy-dom/global-registrator";

GlobalRegistrator.register({ url: "http://localhost/" });

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
