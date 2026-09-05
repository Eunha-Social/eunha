import { expect, test } from '@playwright/test'

const tag = (name: string, uses: number, accounts: number) => ({
  id: name,
  name,
  url: `https://example.invalid/tags/${name}`,
  history: [{ day: '1788566400', uses: String(uses), accounts: String(accounts) }],
  following: false,
})

// Explore is public: trends need no token, and a signed-out visitor has little
// else to browse. Each pane also has to fetch only its own endpoint — three
// hooks mounted at once would pull all three feeds on arrival.
test('explore shows trends to a signed-out visitor, one endpoint per tab', async ({
  page,
}) => {
  const called: string[] = []

  await page.route('**/api/v1/trends/tags*', (route) => {
    called.push('tags')
    return route.fulfill({ json: [tag('japan', 8, 3), tag('rust', 1, 1)] })
  })
  await page.route('**/api/v1/trends/links*', (route) => {
    called.push('links')
    return route.fulfill({ json: [] })
  })
  await page.route('**/api/v1/trends/statuses*', (route) => {
    called.push('statuses')
    return route.fulfill({ json: [] })
  })

  await page.goto('/explore')
  await expect(page.getByText('No posts are trending yet.')).toBeVisible()
  expect(new Set(called)).toEqual(new Set(['statuses']))

  await page.getByRole('link', { name: 'Hashtags' }).click()
  await expect(page.getByRole('link', { name: /#japan/ })).toBeVisible()
  // Plural and singular, and the "from N people" clause only when it is worth
  // saying — a tag used by one account does not name a crowd.
  await expect(page.getByText('8 posts this week from 3 people')).toBeVisible()
  await expect(page.getByText('1 post this week', { exact: true })).toBeVisible()
  expect(new Set(called)).toEqual(new Set(['statuses', 'tags']))

  await page.getByRole('link', { name: 'Links' }).click()
  await expect(page.getByText('No links are trending yet.')).toBeVisible()
  expect(new Set(called)).toEqual(new Set(['statuses', 'tags', 'links']))
})
