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
  await page.route('**/api/v1/notifications**', (r) => r.fulfill({ json: [] }))
  await page.route('**/api/v1/conversations**', (r) => r.fulfill({ json: [] }))
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

  // Only what is missing is offered back.
  await page.getByRole('button', { name: 'Add a timeline' }).click()
  await expect(page.getByRole('menuitem')).toContainText(['Local'])
})

// It opens on the two panes a second column is *for* — the feed and what is
// addressed to you — plus one more to choose.
test('the advanced layout opens on timeline, notifications and one more', async ({
  page,
}) => {
  await signedIn(page)
  await page.goto('/settings')
  await page.getByRole('switch').first().click()
  await page.goto('/')

  const titles = page.locator('.advanced-pane > header button').first()
  await expect(titles).toHaveText('Following')
  await expect(page.locator('.advanced-pane')).toHaveCount(3)
  await expect(
    page
      .locator('.advanced-pane')
      .nth(1)
      .getByRole('button', { name: 'Notifications', exact: true }),
  ).toBeVisible()
})

// The rail's default `left` follows a centred reading column, which this
// layout does not have — unpinned, it lands on top of the first pane on any
// wide screen. Checked at a width where the old positioning overlapped by
// 200px.
test('the rail does not overlap the first pane on a wide screen', async ({ page }) => {
  await signedIn(page)
  await page.goto('/settings')
  await page.getByRole('switch').first().click()

  for (const width of [1280, 1600, 2200]) {
    await page.setViewportSize({ width, height: 900 })
    await page.goto('/')
    const rail = await page.locator('aside.sidebar-frame').boundingBox()
    const panes = await page.locator('.advanced-frame').boundingBox()
    expect(rail, `rail missing at ${width}`).not.toBeNull()
    expect(panes, `panes missing at ${width}`).not.toBeNull()
    expect(rail!.x + rail!.width, `overlap at ${width}px`).toBeLessThanOrEqual(panes!.x)
  }
})

// The rail is fixed, so it cannot be centred by a flow it is not in. Rail and
// panes are placed together from one measured number: centred while the group
// fits, pinned once it does not.
test('the rail and panes are centred together while they fit', async ({ page }) => {
  await signedIn(page)
  await page.goto('/settings')
  await page.getByRole('switch').first().click()

  const edges = async () => {
    const rail = await page.locator('aside.sidebar-frame').boundingBox()
    const gap = await page.evaluate(() => {
      const row = document.querySelector('.advanced-frame')
      if (!row) return null
      const style = getComputedStyle(row)
      const pad =
        (parseFloat(style.paddingLeft) || 0) + (parseFloat(style.paddingRight) || 0)
      const kids = Array.from(row.children) as HTMLElement[]
      const content =
        kids.reduce((sum, k) => sum + k.offsetWidth, 0) +
        (parseFloat(style.columnGap) || 0) * (kids.length - 1) +
        pad
      const left = row.getBoundingClientRect().left
      return window.innerWidth - (left + content)
    })
    return { left: rail!.x, right: gap! }
  }

  // Wide enough for rail plus three panes: even margins either side.
  await page.setViewportSize({ width: 2000, height: 900 })
  await page.goto('/')
  let e = await edges()
  expect(Math.abs(e.left - e.right), 'not centred at 2000px').toBeLessThan(4)
  expect(e.left, 'centred group should not be pinned').toBeGreaterThan(20)

  // Too narrow for the group: pinned left, and the panes scroll instead.
  await page.setViewportSize({ width: 1300, height: 900 })
  await page.goto('/')
  e = await edges()
  expect(Math.round(e.left), 'should pin once it stops fitting').toBe(16)

  // And it follows a resize, not just a fresh load.
  await page.setViewportSize({ width: 2000, height: 900 })
  await expect
    .poll(async () => {
      const after = await edges()
      return Math.abs(after.left - after.right) < 4
    })
    .toBe(true)
})
