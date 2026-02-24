import { type Page } from '@playwright/test'
import * as fixtures from './fixtures/data'

const API_BASE = 'http://localhost:9090'

/** Intercept all API calls with fixture data */
export async function mockApi(page: Page) {
  // Health
  await page.route(`${API_BASE}/health`, (route) =>
    route.fulfill({ json: fixtures.health })
  )

  // Projects
  await page.route(`${API_BASE}/projects`, (route) => {
    if (route.request().method() === 'GET') {
      return route.fulfill({ json: fixtures.projects })
    }
    return route.fulfill({ json: fixtures.projects[0] })
  })

  await page.route(`${API_BASE}/projects/*/specs`, (route) =>
    route.fulfill({ json: fixtures.specs })
  )

  await page.route(`${API_BASE}/projects/*/operations`, (route) =>
    route.fulfill({ json: { specs: [] } })
  )

  await page.route(`${API_BASE}/projects/*/compilations*`, (route) =>
    route.fulfill({ json: [] })
  )

  await page.route(`${API_BASE}/projects/*/artifacts*`, (route) =>
    route.fulfill({ json: [] })
  )

  await page.route(`${API_BASE}/projects/*/data-planes*`, (route) =>
    route.fulfill({ json: [] })
  )

  await page.route(`${API_BASE}/projects/*/api-keys*`, (route) =>
    route.fulfill({ json: [] })
  )

  await page.route(`${API_BASE}/projects/*/plugins*`, (route) =>
    route.fulfill({ json: [] })
  )

  await page.route(`${API_BASE}/projects/*`, (route) => {
    if (route.request().method() === 'GET') {
      return route.fulfill({ json: fixtures.projects[0] })
    }
    return route.continue()
  })

  // Global specs
  await page.route(`${API_BASE}/specs`, (route) => {
    if (route.request().method() === 'GET') {
      return route.fulfill({ json: fixtures.specs })
    }
    // Upload returns a spec with warnings
    return route.fulfill({ json: { ...fixtures.specs[0], warnings: [] } })
  })

  await page.route(`${API_BASE}/specs/*/content`, (route) =>
    route.fulfill({ body: fixtures.specContent, contentType: 'text/plain' })
  )

  await page.route(`${API_BASE}/specs/*/compliance`, (route) =>
    route.fulfill({ json: [] })
  )

  await page.route(`${API_BASE}/specs/*`, (route) => {
    if (route.request().method() === 'DELETE') {
      return route.fulfill({ status: 204 })
    }
    return route.fulfill({ json: fixtures.specs[0] })
  })

  // Plugins
  await page.route(`${API_BASE}/plugins*`, (route) =>
    route.fulfill({ json: fixtures.plugins })
  )

  // Artifacts
  await page.route(`${API_BASE}/artifacts*`, (route) =>
    route.fulfill({ json: [] })
  )

  // Dashboard stats
  await page.route(`${API_BASE}/stats*`, (route) =>
    route.fulfill({
      json: { projects: 1, specs: 1, plugins: 2, artifacts: 0 },
    })
  )
}

/** Perform mock login by setting localStorage auth directly */
export async function login(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem(
      'barbacane-auth',
      JSON.stringify({ id: '1', email: 'admin@barbacane.dev', name: 'admin' })
    )
  })
}
