//! GraphQL operation strings copied verbatim from the Web API implementation
//! (packages/thundoku-api/src/routes/{books,samples,events}.ts and
//! src/lib/checklist-graphql.ts). Do not reformat the query text.

/// books.ts:462 — verbatim.
pub(crate) const BOOKSHELF_QUERY: &str = "query BookShelfQuery($first: Int!, $after: String) { viewer { id bookShelfItems(first: $first, after: $after) { pageInfo { hasNextPage endCursor } edges { node { id causedAt product { databaseID name organization { name } coverImage { url } downloadContent { fileName downloadURL } } marketHandshake { event { id name } } } cursor } } } }";

/// checklist-graphql.ts:78-192 `buildChecklistGraphQLQuery()` — verbatim.
pub(crate) const CHECKLIST_QUERY: &str = r#"
    query EventOfflineCircleChecklistQuery(
      $checkedProductInfosFirst: Int!
      $checkedProductInfosAfter: String
      $followingOrganizationsFirst: Int!
      $followingOrganizationsAfter: String
      $eventID: ID!
    ) {
      viewer {
        id
        checkedProductInfos(
          first: $checkedProductInfosFirst
          after: $checkedProductInfosAfter
        ) {
          pageInfo { hasNextPage endCursor __typename }
          edges {
            node {
              id
              createdAt
              productInfo {
                id
                name
                databaseID
                firstAppearanceEventName
                loginUserBookShelfItem { id __typename }
                coverImage { url __typename }
                productVariants(first: 10) {
                  edges {
                    node {
                      id
                      kind
                      price
                      status
                      __typename
                    }
                  }
                  __typename
                }
                organization {
                  id
                  name
                  circles(first: 1, criteria: {eventID: $eventID}) {
                    pageInfo { hasNextPage endCursor __typename }
                    edges {
                        node {
                          id
                          databaseID
                          spaces
                          hasOfflineCourse
                          event { id databaseID __typename }
                          __typename
                        }
                      cursor
                      __typename
                    }
                    __typename
                  }
                  __typename
                }
                __typename
              }
              __typename
            }
            cursor
            __typename
          }
          __typename
        }
        followingOrganizations(
          first: $followingOrganizationsFirst
          after: $followingOrganizationsAfter
        ) {
          pageInfo { hasNextPage endCursor __typename }
          edges {
            cursor
            node {
              id
              organization {
                id
                name
                image { id url width height __typename }
                circles(first: 1, criteria: {eventID: $eventID}) {
                  pageInfo { hasNextPage endCursor __typename }
                  edges {
                    node {
                      id
                      databaseID
                      spaces
                      hasOfflineCourse
                      event { id databaseID __typename }
                      __typename
                    }
                    cursor
                    __typename
                  }
                  __typename
                }
                __typename
              }
              __typename
            }
            __typename
          }
          __typename
        }
        __typename
      }
      event(id: $eventID) {
        id
        __typename
      }
    }
"#;

/// samples.ts:97-115 — verbatim.
pub(crate) const PRODUCT_IMAGES_QUERY: &str = r#"
    query ProductImagesQuery($productInfoID: ID!) {
      product(id: $productInfoID) {
        id
        databaseID
        images(first: 8) {
          edges {
            node {
              id
              databaseID
              url
              width
              height
            }
          }
        }
      }
    }
"#;

/// events.ts:96 `buildGraphQLQuery()` — verbatim.
pub(crate) const EVENT_QUERY: &str =
    "query TbfEventQuery($eventID: ID!) { event(id: $eventID) { id databaseID name } }";

/// Captured live on 2026-08-21 (browser): the login mutation the
/// techbookfest.org SPA itself sends.
pub(crate) const LOGIN_MUTATION: &str = "mutation UserLoginMutation($loginInput: LoginUserInput!) { loginUser(input: $loginInput) { user { id ...GetMeUserFragment __typename } __typename } } fragment GetMeUserFragment on User { id email __typename }";
