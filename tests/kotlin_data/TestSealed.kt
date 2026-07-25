package pkg

sealed class Result {
    data class Success(val value: String) : Result()
    data class Error(val message: String) : Result()
    object Loading : Result()
}
