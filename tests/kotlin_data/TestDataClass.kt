package pkg

data class Person(val name: String, val age: Int) {
    fun greet(): String = "Hello, $name!"
}
