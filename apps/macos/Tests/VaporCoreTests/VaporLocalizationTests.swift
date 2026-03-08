import Foundation
import Testing

@testable import VaporCore

@Test
func resolveUsesEnglishCatalogByDefault() {
  let store = VaporLocalizationStore(preferredLanguagesProvider: { ["en-US"] })

  let catalog = store.resolve(preferredLanguageCodeOverride: nil)

  #expect(catalog.effectiveLanguageCode == "en")
  #expect(catalog.availableLanguageCodes.contains("en"))
  #expect(catalog.text("app_title") == "Vapor")
}

@Test
func resolveFallsBackToEnglishWhenOverrideLanguageMissing() {
  let store = VaporLocalizationStore(preferredLanguagesProvider: { ["de-DE"] })

  let catalog = store.resolve(preferredLanguageCodeOverride: "es")

  #expect(catalog.effectiveLanguageCode == "en")
  #expect(catalog.text("settings_language") == "Language")
}

@Test
func missingTranslationKeyFallsBackToKeyName() {
  let store = VaporLocalizationStore(preferredLanguagesProvider: { ["en-US"] })

  let catalog = store.resolve(preferredLanguageCodeOverride: nil)

  #expect(catalog.text("unknown_key") == "unknown_key")
}
