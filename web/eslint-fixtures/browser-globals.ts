// Each line below must be refused outside src/platform/.
export const all = [
  window,
  document,
  localStorage,
  sessionStorage,
  location,
  navigator,
  Notification,
  IntersectionObserver,
];

export const copy = navigator.clipboard.writeText("code");
