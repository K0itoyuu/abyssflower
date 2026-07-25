package pkg

// Test various Kotlin patterns for decompilation

class Patterns {
    // Elvis operator
    fun getLength(s: String?): Int {
        return s?.length ?: 0
    }

    // Safe call
    fun safeUpper(s: String?): String? {
        return s?.uppercase()
    }

    // For-in loop
    fun sumList(items: List<Int>): Int {
        var sum = 0
        for (item in items) {
            sum += item
        }
        return sum
    }

    // Range
    fun rangeSum(): Int {
        var sum = 0
        for (i in 1..10) {
            sum += i
        }
        return sum
    }

    // Lambda
    fun mapNames(names: List<String>): List<Int> {
        return names.map { it.length }
    }

    // Destructuring
    fun swap(pair: Pair<Int, Int>): Pair<Int, Int> {
        val (a, b) = pair
        return Pair(b, a)
    }

    // Property with custom getter
    val isEmpty: Boolean
        get() = items.isEmpty()

    private val items = mutableListOf<String>()

    // Let scope function
    fun processName(name: String?): String {
        return name?.let { it.trim().uppercase() } ?: "UNKNOWN"
    }
}
