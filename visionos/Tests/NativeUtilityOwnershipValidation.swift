import Foundation

@main struct NativeUtilityOwnershipValidation {
    static func main() {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message); checks += 1
        }
        var ownership = NativeSenseUtilityOwnership()
        require(!ownership.interactionActive && ownership.directions.isEmpty, "No gesture owns game input initially")
        ownership.setFlat(.down, pressed: true)
        ownership.setFlat(.up, pressed: true)
        require(ownership.interactionActive && ownership.directions == [.down, .up], "Two flat buttons are independent held owners")
        let before = ownership.directions
        ownership.setFlat(.down, pressed: false)
        require(ownership.interactionActive && ownership.directions == [.up], "Releasing flat Down keeps flat Up and input ownership")
        require(before.subtracting(ownership.directions) == [.down], "Native direction delta releases only the ended button")
        ownership.setFlat(.phone, pressed: true)
        ownership.setFlat(.up, pressed: false)
        require(ownership.interactionActive && ownership.directions.isEmpty, "Phone still owns physical input after the last direction releases")
        ownership.setFlat(.phone, pressed: false)
        require(!ownership.interactionActive, "Last flat button releases ownership")
        ownership.setFlat(.down, pressed: true)
        ownership.setImmersiveActive(true)
        ownership.setImmersiveDirection(.down, pressed: true)
        let shared = ownership.directions
        ownership.setFlat(.down, pressed: false)
        require(ownership.directions == [.down] && shared.subtracting(ownership.directions).isEmpty,
                "Same immersive direction prevents an incorrect native key-up on flat release")
        ownership.setFlat(.left, pressed: true)
        ownership.clearFlat()
        require(ownership.interactionActive && ownership.directions == [.down],
                "Flat disappearance releases flat actions only; immersive hold survives")
        ownership.setFlat(.right, pressed: true)
        ownership.setImmersiveActive(false)
        require(ownership.interactionActive && ownership.directions == [.right],
                "Immersive stop releases immersive source only; active flat hold survives")
        ownership.clear()
        require(!ownership.interactionActive && ownership.flatActions.isEmpty && ownership.directions.isEmpty,
                "Global lifecycle loss clears every source and key")
        ownership.setFlat(.left, pressed: true)
        ownership.setFlat(.left, pressed: true)
        ownership.setFlat(.left, pressed: false)
        ownership.setFlat(.left, pressed: false)
        require(!ownership.interactionActive && ownership.directions.isEmpty, "Duplicate press/release state is idempotent")
        ownership.setImmersiveDirection(.phone, pressed: true)
        require(ownership.directions.isEmpty && !ownership.interactionActive, "Non-direction cannot become a held arrow")
        ownership.setImmersiveActive(true)
        require(ownership.interactionActive && ownership.directions.isEmpty, "Immersive Phone/View gesture can own input without a direction")
        ownership.setImmersiveActive(false)
        require(!ownership.interactionActive, "Immersive non-direction gesture release clears its ownership")
        print("PASS flat/immersive actual source ownership: \(checks) checks")
    }
}
