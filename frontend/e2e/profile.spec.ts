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

// Muting from a profile. eunha's mute is narrower than Mastodon's — a muted
// account can still mention you and react to your posts — so the page has to
// say so, or the button promises something it does not do.
const account = {
  id: '2',
  username: 'bob',
  acct: 'bob',
  display_name: 'Bob',
  note: '',
  url: 'https://example.invalid/@bob',
  uri: 'https://example.invalid/users/bob',
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
  discoverable: true,
  indexable: true,
}

const relationship = (muting: boolean) => ({
  id: '2',
  following: true,
  showing_reblogs: true,
  notifying: false,
  followed_by: false,
  blocking: false,
  blocked_by: false,
  muting,
  muting_notifications: muting,
  requested: false,
  domain_blocking: false,
  endorsed: false,
  note: '',
})

test('a profile can be muted, and says what a mute does', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('eunha:token', 'test-token')
  })

  let muted = false
  let muteCalls = 0

  await page.route('**/api/v1/accounts/lookup**', (route) =>
    route.fulfill({ json: account }),
  )
  await page.route('**/api/v1/accounts/verify_credentials**', (route) =>
    route.fulfill({ json: { ...account, id: '1', username: 'alice', acct: 'alice' } }),
  )
  await page.route('**/api/v1/accounts/relationships**', (route) =>
    route.fulfill({ json: [relationship(muted)] }),
  )
  await page.route('**/api/v1/accounts/2/statuses**', (route) =>
    route.fulfill({ json: [] }),
  )
  await page.route('**/api/v1/accounts/2/mute', async (route) => {
    muteCalls += 1
    muted = true
    await route.fulfill({ json: relationship(true) })
  })

  await page.goto('/@bob')
  await expect(page.getByRole('button', { name: 'Following' })).toBeVisible()

  await page.getByRole('button', { name: 'More actions for @bob' }).click()
  await page.getByRole('menuitem', { name: 'Mute' }).click()

  // The confirmation names the limits of the mute rather than just reporting it.
  await expect(
    page.getByText(/^Muted @bob\..*still mention you and react to your posts/),
  ).toBeVisible()
  expect(muteCalls).toBe(1)

  // And the profile keeps saying so, for anyone arriving later.
  await expect(
    page.getByText(/Muted — their posts are hidden from your timelines/),
  ).toBeVisible()
  await page.getByRole('button', { name: 'More actions for @bob' }).click()
  await expect(page.getByRole('menuitem', { name: 'Unmute' })).toBeVisible()
})
