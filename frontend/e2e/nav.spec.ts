import { expect, test } from '@playwright/test'

test('timeline tabs navigate to public timelines', async ({ page }) => {
  await page.goto('/')

  await page.getByRole('link', { name: 'Federated' }).click()
  await expect(page).toHaveURL(/\/public$/)

  await page.getByRole('link', { name: 'Local' }).click()
  await expect(page).toHaveURL(/\/local$/)
})
