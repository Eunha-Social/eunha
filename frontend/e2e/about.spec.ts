import { expect, test } from '@playwright/test'

const base = {
  domain: 'example.invalid',
  title: 'Example',
  version: '0.2.0 (compatible; Mastodon 4.7.1)',
  source_url: 'https://github.com/limeburst/eunha',
  description: 'A place.',
  usage: { users: { active_month: 4 } },
  languages: ['ko', 'en'],
  configuration: {},
  registrations: { enabled: false, approval_required: false },
  contact: { email: 'admin@example.invalid', account: null },
  rules: [],
  api_versions: {},
}

// Every one of these is optional on the wire, and seoul.earth has none of the
// policy documents configured. An instance that publishes nothing must not get
// a page of empty headings — so the test that matters is which sections are
// absent, not which are present.
test('about renders only the sections the instance actually publishes', async ({
  page,
}) => {
  await page.route('**/api/v2/instance', (r) => r.fulfill({ json: base }))
  await page.route('**/api/v1/instance/privacy_policy', (r) =>
    r.fulfill({ json: { updated_at: '', content: '' } }),
  )
  await page.route('**/api/v1/instance/terms_of_service', (r) => r.fulfill({ json: [] }))

  await page.goto('/about')
  await expect(page.getByRole('heading', { name: 'Example' })).toBeVisible()
  await expect(page.getByText('Closed — new accounts are by invitation only.')).toBeVisible()
  await expect(page.getByText('4 people have posted in the last month.')).toBeVisible()

  await expect(page.getByRole('heading', { name: 'Privacy' })).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Terms of service' })).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Rules' })).toHaveCount(0)
  // No contact account was returned, so there is nobody to name.
  await expect(page.getByRole('heading', { name: 'Run by' })).toHaveCount(0)
})

test('about shows the policy documents when an instance has them', async ({ page }) => {
  await page.route('**/api/v2/instance', (r) =>
    r.fulfill({
      json: {
        ...base,
        registrations: { enabled: true, approval_required: true },
        rules: [{ id: '1', text: 'Be decent to each other.' }],
      },
    }),
  )
  await page.route('**/api/v1/instance/privacy_policy', (r) =>
    r.fulfill({ json: { updated_at: '', content: 'We keep your posts.' } }),
  )
  // Terms come back as a list of versions by effective date, not an object.
  await page.route('**/api/v1/instance/terms_of_service', (r) =>
    r.fulfill({ json: [{ effective_date: '2025-01-01', effective: true, content: 'Play nice.' }] }),
  )

  await page.goto('/about')
  await expect(
    page.getByText('Open, and each new account is reviewed before it can sign in.'),
  ).toBeVisible()
  await expect(page.getByText('Be decent to each other.')).toBeVisible()
  await expect(page.getByText('We keep your posts.')).toBeVisible()
  await expect(page.getByText('Play nice.')).toBeVisible()
})
