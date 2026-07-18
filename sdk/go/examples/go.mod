// Separate module on purpose: the Anthropic SDK is an example-only dependency,
// so github.com/w3-surfer/cloudiy/sdk/go itself stays zero-dependency.
module github.com/w3-surfer/cloudiy/sdk/go/examples

go 1.23.0

require (
	github.com/anthropics/anthropic-sdk-go v1.13.0
	github.com/w3-surfer/cloudiy/sdk/go v0.0.0
)

require (
	github.com/tidwall/gjson v1.18.0 // indirect
	github.com/tidwall/match v1.1.1 // indirect
	github.com/tidwall/pretty v1.2.1 // indirect
	github.com/tidwall/sjson v1.2.5 // indirect
)

replace github.com/w3-surfer/cloudiy/sdk/go => ../
