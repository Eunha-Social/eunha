import { expect, test } from '@playwright/test'

// A suspended or deleted account is still served by the API — blanked, with
// `suspended: true` — so the profile page has to say so rather than render an
// empty shell. Stubbed here; no backend needed.
test('a suspended account shows a tombstone instead of a profile', async ({
  page,
}) => {
  await page.route('**/api/v1/accounts/lookup**', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        id: '1',
        username: 'ghost',
        acct: 'ghost',
        display_name: '',
        note: '',
        url: 'https://example.invalid/@ghost',
        uri: 'https://example.invalid/users/ghost',
        avatar: '/missing.png',
        avatar_static: '/missing.png',
        header: '/missing.png',
        header_static: '/missing.png',
        followers_count: 0,
        following_count: 0,
        statuses_count: 0,
        created_at: '2026-08-12T00:00:00.000Z',
        emojis: [],
        fields: [],
        locked: false,
        bot: false,
        group: false,
        discoverable: false,
        indexable: false,
        suspended: true,
      }),
    }),
  )

  await page.goto('/@ghost')

  await expect(
    page.getByRole('heading', { name: 'Account suspended' }),
  ).toBeVisible()
  // None of the normal profile furniture should be there.
  await expect(page.getByRole('button', { name: 'Follow' })).toBeHidden()
})
