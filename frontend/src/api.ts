// Mastodon C2S API calls, backed by masto.js. The same API is consumed by
// third-party mobile apps; this frontend is just one client.
import { restClient, type mastodon } from './masto.ts'

export function getInstance(): Promise<mastodon.v2.Instance> {
  return restClient().v2.instance.fetch()
}

export async function getHomeTimeline(
  token: string,
  maxId?: string,
): Promise<mastodon.v1.Status[]> {
  // The paginator is awaitable and resolves to the first page.
  return restClient(token).v1.timelines.home.list({ limit: 40, maxId })
}

export function postStatus(
  token: string,
  params: {
    status: string
    visibility?: mastodon.v1.StatusVisibility
    inReplyToId?: string
    quotedStatusId?: string
    mediaIds?: string[]
  },
): Promise<mastodon.v1.Status> {
  const { status, visibility, inReplyToId, quotedStatusId, mediaIds } = params
  // With media, masto requires the media-ids variant (status optional).
  if (mediaIds && mediaIds.length > 0) {
    return restClient(token).v1.statuses.create({
      status,
      visibility,
      inReplyToId,
      quotedStatusId,
      mediaIds,
    })
  }
  return restClient(token).v1.statuses.create({
    status,
    visibility,
    inReplyToId,
    quotedStatusId,
  })
}

export function uploadMedia(
  file: File,
  token: string,
  description?: string,
): Promise<mastodon.v1.MediaAttachment> {
  return restClient(token).v2.media.create({ file, description })
}

export function updateMediaDescription(
  id: string,
  description: string,
  token: string,
): Promise<mastodon.v1.MediaAttachment> {
  return restClient(token).v1.media.$select(id).update({ description })
}

// Status interactions. Each returns the updated status; the caller normalizes
// reblog wrappers (a boost response wraps the original in `.reblog`).
export function setFavourite(token: string, id: string, on: boolean) {
  const s = restClient(token).v1.statuses.$select(id)
  return on ? s.favourite() : s.unfavourite()
}

export function setReblog(token: string, id: string, on: boolean) {
  const s = restClient(token).v1.statuses.$select(id)
  return on ? s.reblog() : s.unreblog()
}

export function setBookmark(token: string, id: string, on: boolean) {
  const s = restClient(token).v1.statuses.$select(id)
  return on ? s.bookmark() : s.unbookmark()
}

export function getCurrentAccount(token: string): Promise<mastodon.v1.AccountCredentials> {
  return restClient(token).v1.accounts.verifyCredentials()
}

export function updateAccountImages(
  token: string,
  params: { avatar?: File; header?: File },
): Promise<mastodon.v1.AccountCredentials> {
  return restClient(token).v1.accounts.updateCredentials(params)
}

export function updateAccountProfile(
  token: string,
  params: {
    displayName?: string
    note?: string
    fieldsAttributes?: { name: string; value: string }[]
  },
): Promise<mastodon.v1.AccountCredentials> {
  return restClient(token).v1.accounts.updateCredentials(params)
}

export async function getFeaturedTags(
  token: string,
): Promise<mastodon.v1.FeaturedTag[]> {
  return restClient(token).v1.featuredTags.list()
}

export async function getFeaturedTagSuggestions(
  token: string,
): Promise<mastodon.v1.Tag[]> {
  return restClient(token).v1.featuredTags.suggestions.list()
}

export function createFeaturedTag(
  token: string,
  name: string,
): Promise<mastodon.v1.FeaturedTag> {
  return restClient(token).v1.featuredTags.create({ name })
}

export function deleteFeaturedTag(token: string, id: string): Promise<void> {
  return restClient(token).v1.featuredTags.$select(id).remove()
}

export function deleteProfileAvatar(token: string): Promise<mastodon.v1.Account> {
  return restClient(token).v1.profile.avatar.remove()
}

export function deleteProfileHeader(token: string): Promise<mastodon.v1.Account> {
  return restClient(token).v1.profile.header.remove()
}

export function lookupAccount(
  acct: string,
  token?: string,
): Promise<mastodon.v1.Account> {
  return restClient(token).v1.accounts.lookup({ acct })
}

export async function getAccountStatuses(
  id: string,
  token?: string,
  maxId?: string,
): Promise<mastodon.v1.Status[]> {
  return restClient(token).v1.accounts.$select(id).statuses.list({ limit: 40, maxId })
}

export function getStatus(id: string, token?: string): Promise<mastodon.v1.Status> {
  return restClient(token).v1.statuses.$select(id).fetch()
}

export function getStatusContext(
  id: string,
  token?: string,
): Promise<mastodon.v1.Context> {
  return restClient(token).v1.statuses.$select(id).context.fetch()
}

export function deleteStatus(id: string, token: string): Promise<mastodon.v1.Status> {
  return restClient(token).v1.statuses.$select(id).remove()
}

export function getStatusSource(
  id: string,
  token: string,
): Promise<mastodon.v1.StatusSource> {
  return restClient(token).v1.statuses.$select(id).source.fetch()
}

export function updateStatus(
  id: string,
  params: { status: string; spoilerText?: string },
  token: string,
): Promise<mastodon.v1.Status> {
  return restClient(token).v1.statuses.$select(id).update(params)
}

export async function getRelationship(
  id: string,
  token: string,
): Promise<mastodon.v1.Relationship | undefined> {
  const rels = await restClient(token).v1.accounts.relationships.fetch({ id: [id] })
  return rels[0]
}

export function setFollow(
  id: string,
  token: string,
  on: boolean,
  params?: { reblogs?: boolean | null },
) {
  const a = restClient(token).v1.accounts.$select(id)
  return on ? a.follow(params) : a.unfollow(params)
}

export async function getPublicTimeline(
  local: boolean,
  token?: string,
  maxId?: string,
): Promise<mastodon.v1.Status[]> {
  return restClient(token).v1.timelines.public.list({ local, limit: 40, maxId })
}

export async function getNotifications(
  token: string,
  maxId?: string,
): Promise<mastodon.v1.Notification[]> {
  return restClient(token).v1.notifications.list({ limit: 40, maxId })
}

export function votePoll(
  pollId: string,
  choices: number[],
  token: string,
): Promise<mastodon.v1.Poll> {
  return restClient(token).v1.polls.$select(pollId).votes.create({ choices })
}

export async function search(
  q: string,
  token?: string,
): Promise<mastodon.v2.Search> {
  // resolve remote accounts/statuses only for authenticated searches.
  return restClient(token).v2.search.list({ q, resolve: !!token, limit: 20 })
}

export async function getTagTimeline(
  name: string,
  token?: string,
  maxId?: string,
): Promise<mastodon.v1.Status[]> {
  return restClient(token).v1.timelines.tag.$select(name).list({ limit: 40, maxId })
}

export async function searchAccounts(
  q: string,
  token: string,
  limit = 6,
): Promise<mastodon.v1.Account[]> {
  // Mention autocomplete: no WebFinger resolution (fast, local/known accounts).
  return restClient(token).v1.accounts.search.list({ q, limit, resolve: false })
}

export async function getFollowRequests(
  token: string,
  maxId?: string,
): Promise<mastodon.v1.Account[]> {
  return restClient(token).v1.followRequests.list({ limit: 40, maxId })
}

export function authorizeFollowRequest(
  id: string,
  token: string,
): Promise<mastodon.v1.Relationship> {
  return restClient(token).v1.followRequests.$select(id).authorize()
}

export function rejectFollowRequest(
  id: string,
  token: string,
): Promise<mastodon.v1.Relationship> {
  return restClient(token).v1.followRequests.$select(id).reject()
}

export async function getFollowers(
  id: string,
  token?: string,
  maxId?: string,
): Promise<mastodon.v1.Account[]> {
  return restClient(token).v1.accounts.$select(id).followers.list({ limit: 40, maxId })
}

export async function getFollowing(
  id: string,
  token?: string,
  maxId?: string,
): Promise<mastodon.v1.Account[]> {
  return restClient(token).v1.accounts.$select(id).following.list({ limit: 40, maxId })
}
