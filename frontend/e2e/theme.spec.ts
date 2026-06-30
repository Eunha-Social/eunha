import { expect, test } from '@playwright/test'

test('theme switcher opens and toggles dark mode', async ({ page }) => {
  await page.goto('/')
  const html = page.locator('html')

  // Open the dropdown (regression guard: this stayed hidden on React 18).
  await page.getByRole('button', { name: 'Toggle theme' }).click()
  await expect(page.getByRole('menuitem', { name: 'Dark' })).toBeVisible()

  // Switch to Dark.
  await page.getByRole('menuitem', { name: 'Dark' }).click()
  await expect(html).toHaveClass(/dark/)

  // Switch back to Light.
  await page.getByRole('button', { name: 'Toggle theme' }).click()
  await page.getByRole('menuitem', { name: 'Light' }).click()
  await expect(html).not.toHaveClass(/dark/)
})

test('renders the header with a sign-in action when logged out', async ({
  page,
}) => {
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible()
})
