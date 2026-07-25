package pkg

fun String.wordCount(): Int = this.split(" ").size

suspend fun fetchData(url: String): String = url

inline fun <reified T> List<T>.filterIsInstance(): List<T> = this
