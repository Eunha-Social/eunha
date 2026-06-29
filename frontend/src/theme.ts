// Follow the OS color scheme by toggling shadcn's `.dark` class on <html>.
// A manual theme switcher can later override this by setting the class directly.
export function initTheme() {
  const mq = window.matchMedia('(prefers-color-scheme: dark)')
  const apply = () => document.documentElement.classList.toggle('dark', mq.matches)
  apply()
  mq.addEventListener('change', apply)
}
