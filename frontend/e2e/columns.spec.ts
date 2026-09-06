import { expect, test } from '@playwright/test'

const me = {
  id: '1',
  username: 'alice',
  acct: 'alice',
  display_name: 'Alice',
  avatar: '',
  source: { privacy: 'public' },
}

async function signedIn(page: import('@playwright/test').Page) {
  await page.addInitScript(() => localStorage.setItem('eunha:token', 'test-token'))
  await page.route('**/api/v1/accounts/verify_credentials**', (r) => r.fulfill({ json: me }))
  await page.route('**/api/v1/notifications/unread_count**', (r) =>
    r.fulfill({ json: { count: 0 } }),
  )
  await page.route('**/api/v1/timelines/**', (r) => r.fulfill({ json: [] }))
}

// The nav says Home; the column says what the feed is. Both are deliberate,
// and the accessible label stays "Home" so the two never disagree for a screen
// reader.
test('the home column is headed Following', async ({ page }) => {
  await signedIn(page)
  await page.goto('/')

  const column = page.locator('.column-frame')
  await expect(column.getByRole('button', { name: 'Following' })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Home' })).toBeVisible()
  // And the rail still calls it Home.
  await expect(page.locator('aside').getByRole('link', { name: 'Home' })).toBeVisible()
})

test('local and federated columns are headed by their feed', async ({ page }) => {
  await signedIn(page)
  await page.goto('/local')
  await expect(
    page.locator('.column-frame').getByRole('button', { name: 'Local' }),
  ).toBeVisible()

  await page.goto('/public')
  await expect(
    page.locator('.column-frame').getByRole('button', { name: 'Federated' }),
  ).toBeVisible()
})

// The advanced layout is off unless asked for, which is the whole point of
// gating it — the default stays one column.
test('the advanced layout is opt-in and remembers its panes', async ({ page }) => {
  await signedIn(page)
  await page.goto('/')
  await expect(page.locator('.advanced-pane')).toHaveCount(0)

  await page.goto('/settings')
  await page.getByRole('switch').first().click()

  await page.goto('/')
  await expect(page.locator('.advanced-pane')).toHaveCount(3)

  // Closing one sticks across a reload, because it is stored, not just state.
  await page.getByRole('button', { name: 'Close Local' }).click()
  await expect(page.locator('.advanced-pane')).toHaveCount(2)
  await page.reload()
  await expect(page.locator('.advanced-pane')).toHaveCount(2)

  // Only the missing one is offered back.
  await page.getByRole('button', { name: 'Add a timeline' }).click()
  await expect(page.getByRole('menuitem')).toHaveText(['Local'])
})
