import { test, expect } from '@playwright/test'
import { mockApi, login } from './helpers'

test.describe('Spec workflows', () => {
  test.beforeEach(async ({ page }) => {
    await login(page)
    await mockApi(page)
  })

  test('view spec content in side panel', async ({ page }) => {
    await page.goto('/specs')

    // Click on the spec card to view it
    await page.getByText('petstore.yaml').click()

    // Spec content should appear in the preview panel
    await expect(page.getByText('Pet Store')).toBeVisible()
    await expect(page.getByText('listPets')).toBeVisible()
  })

  test('view spec content in project modal', async ({ page }) => {
    const projectId = '11111111-1111-1111-1111-111111111111'
    await page.goto(`/projects/${projectId}/specs`)

    // Click View button
    await page.getByRole('button', { name: 'View' }).click()

    // Modal should show spec content
    await expect(page.getByText('listPets')).toBeVisible()

    // Close the modal
    await page.getByRole('button', { name: 'Close' }).click()
    await expect(page.getByText('listPets')).not.toBeVisible()
  })

  test('delete spec shows confirmation dialog', async ({ page }) => {
    const projectId = '11111111-1111-1111-1111-111111111111'
    await page.goto(`/projects/${projectId}/specs`)

    // Click delete button (trash icon)
    await page.locator('button').filter({ has: page.locator('.lucide-trash-2') }).click()

    // Confirmation dialog should appear
    await expect(page.getByText('Delete spec')).toBeVisible()
    await expect(page.getByText('Are you sure you want to delete')).toBeVisible()

    // Cancel should close the dialog
    await page.getByRole('button', { name: 'Cancel' }).click()
    await expect(page.getByText('Are you sure you want to delete')).not.toBeVisible()
  })

  test('check compliance returns results', async ({ page }) => {
    const projectId = '11111111-1111-1111-1111-111111111111'
    await page.goto(`/projects/${projectId}/specs`)

    // Click the Check button
    await page.getByRole('button', { name: 'Check' }).click()

    // With our mock returning empty warnings, should show success message
    await expect(page.getByText('No compliance warnings found')).toBeVisible()
  })
})
