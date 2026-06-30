import { expect, test } from '@playwright/test'

test('search submits the query into the URL', async ({ page }) => {
  await page.goto('/search')
  await page.getByPlaceholder('Search posts, people, and hashtags').fill('hello')
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/\/search\?q=hello$/)
})
