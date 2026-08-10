package ontodb

import "fmt"

// Error types for the OntoDB SDK.

// ConnectionError indicates a connection failure.
type ConnectionError struct {
	Err error
}

func (e *ConnectionError) Error() string {
	return fmt.Sprintf("ontodb: connection error: %v", e.Err)
}

func (e *ConnectionError) Unwrap() error { return e.Err }

// QueryError indicates a query execution failure.
type QueryError struct {
	Message string
}

func (e *QueryError) Error() string {
	return fmt.Sprintf("ontodb: query error: %s", e.Message)
}

// AuthenticationError indicates an authentication failure.
type AuthenticationError struct {
	Message string
}

func (e *AuthenticationError) Error() string {
	return fmt.Sprintf("ontodb: authentication error: %s", e.Message)
}

// RateLimitError indicates rate limiting.
type RateLimitError struct {
	Message string
}

func (e *RateLimitError) Error() string {
	return fmt.Sprintf("ontodb: rate limit: %s", e.Message)
}

// TimeoutError indicates a request timeout.
type TimeoutError struct {
	Message string
}

func (e *TimeoutError) Error() string {
	return fmt.Sprintf("ontodb: timeout: %s", e.Message)
}
