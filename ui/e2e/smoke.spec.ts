import { test, expect } from '@playwright/test'
import { mockApi, login } from './helpers'

test.describe('Smoke tests', () => {
  test('login page renders and mock login works', async ({ page }) => {
    await page.goto('/login')
    await expect(page.getByText('Sign in to the control plane')).toBeVisible()

    await page.fill('#email', 'admin@barbacane.dev')
    await page.fill('#password', 'password')
    await page.click('button[type="submit"]')

    // After login, should redirect away from login page
    await expect(page).not.toHaveURL(/\/login/)
  })

  test('dashboard loads after login', async ({ page }) => {
    await login(page)
    await mockApi(page)
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Dashboard' })).toBeVisible()
  })

  test('navigate to Projects page', async ({ page }) => {
    await login(page)
    await mockApi(page)
    await page.goto('/projects')
    await expect(page.getByRole('heading', { name: 'Projects' })).toBeVisible()
    // Fixture project should appear
    await expect(page.getByText('Pet Store API')).toBeVisible()
  })

  test('navigate to global Specs page', async ({ page }) => {
    await login(page)
    await mockApi(page)
    await page.goto('/specs')
    await expect(page.getByRole('heading', { name: 'API Specs' })).toBeVisible()
    await expect(page.getByText('petstore.yaml')).toBeVisible()
  })

  test('navigate to Plugin Registry', async ({ page }) => {
    await login(page)
    await mockApi(page)
    await page.goto('/plugin-registry')
    await expect(page.getByRole('heading', { name: 'Plugin Registry' })).toBeVisible()
    await expect(page.getByText('rate-limit')).toBeVisible()
  })

  test('navigate into project tabs', async ({ page }) => {
    await login(page)
    await mockApi(page)
    const projectId = '11111111-1111-1111-1111-111111111111'

    // Specs tab (default)
    await page.goto(`/projects/${projectId}/specs`)
    await expect(page.getByRole('heading', { name: 'API Specifications' })).toBeVisible()

    // Operations tab
    await page.goto(`/projects/${projectId}/operations`)
    await expect(page.getByRole('heading', { name: 'Operations' })).toBeVisible()

    // Builds tab
    await page.goto(`/projects/${projectId}/builds`)
    await expect(page.getByRole('heading', { name: 'Builds' })).toBeVisible()

    // Deploy tab
    await page.goto(`/projects/${projectId}/deploy`)
    await expect(page.getByText('Deploy to Data Planes')).toBeVisible()
  })

  test('unauthenticated user is redirected to login', async ({ page }) => {
    await page.goto('/')
    await expect(page).toHaveURL(/\/login/)
  })
})
